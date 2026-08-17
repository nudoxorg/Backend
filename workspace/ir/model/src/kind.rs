//! `Kind` enum and frozen `KindDiscriminant` wire tags for every kind.
use std::any::Any;

use crate::{kinds::*, visitor::Visitor};

register_kinds! {
    /// A namespace, package, or module — a container for other entries.
    Module = 1,

    /// A product type: struct, class, record, or data class.
    Record = 2,

    /// A field or property of a containing type.
    Field = 3,

    /// A function, method, or lambda.
    Function = 4,

    /// A type alias (or abstract associated-type declaration).
    ///
    /// Covers Rust `type Foo = Bar<u32>;`, C/C++ `using Foo = Bar;`,
    /// TypeScript `type Foo = …`, and associated-type declarations without a
    /// target (`type Item;` / `type Item: Display;`).
    ///
    /// # Naming note
    ///
    /// The Rust identifier is `Alias` (not `Type`) to avoid a name collision
    /// with `crate::kinds::ty::Type`, the type-*expression* enum.  The
    /// wire discriminant **5** is unchanged from the reserved slot.
    Alias = 5,

    /// A trait, interface, or protocol definition.
    Trait = 6,

    /// A trait implementation or inherent impl block.
    Impl = 7,

    /// A sum type: enum, tagged union, or sealed hierarchy.
    Enum = 8,

    /// A single variant of a sum type.
    Variant = 9,

    /// A compile-time constant declaration.
    Const = 10,

    /// A static variable declaration.
    Static = 11,

    /// A re-export (public alias) entry; target is in EntryInner::Reference.
    Reexport = 12,

    /// A single parameter of a function.
    Param = 13,
}

macro_rules! register_kinds {
  ($($(#[$meta:meta])* $kind:ident = $disc:literal,)*) => {
    #[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
    pub enum Kind {
      $(
      $(#[$meta])*
      $kind($kind),
      )*
    }

    impl Kind {
      pub fn discriminant(&self) -> KindDiscriminant {
        match self {
          $(
          Kind::$kind(_) => KindDiscriminant::$kind,
          )*
        }
      }

      pub(crate) fn variant_as_dyn(&self) -> &dyn Any {
        match self {
          $(
          Kind::$kind(it) => it,
          )*
        }
      }
    }

    #[repr(u16)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
    pub enum KindDiscriminant {
      $(
      $(#[$meta])*
      $kind = $disc,
      )*
    }

    impl KindDiscriminant {
      /// Returns the wire-stable `u16` value for this discriminant.
      pub fn as_u16(self) -> u16 {
        self as u16
      }

      /// Reconstruct a `KindDiscriminant` from its wire `u16` value.
      ///
      /// Returns `None` for values that do not correspond to any registered
      /// kind (unrecognised or reserved discriminants).
      pub fn from_u16(v: u16) -> Option<Self> {
        match v {
          $($disc => Some(KindDiscriminant::$kind),)*
          _ => None,
        }
      }
    }

    pub trait EntryKind: Any + private::Sealed {
      fn into_kind(self) -> Kind
      where
        Self: Sized;

      fn discriminant() -> KindDiscriminant
      where
        Self: Sized;
    }

    mod private {
      pub trait Sealed {}
    }

    $(
    impl private::Sealed for $kind {}

    impl EntryKind for $kind {
      fn into_kind(self) -> Kind { Kind::$kind(self) }

      fn discriminant() -> KindDiscriminant { KindDiscriminant::$kind }
    }
    )*
  }
}

use register_kinds;
