use std::any::Any;

use crate::{kinds::*, visitor::Visitor};

// Re-exports (the old `Reexport` Kind) are modelled by `EntryInner::Reference`,
// not a distinct Kind. `Event` (C#) folds into a `Field`/`Function` pair, and
// free-form prose docs (`Info`) are out of scope for the type-system IR.
register_kinds! {
    /// A namespace, package, or module — a container for other entries.
    Module,

    /// A product type: struct, class, record, or data class.
    Record,

    /// A sum type: enum, tagged union, or sealed hierarchy.
    Enum,

    /// A single variant of a sum type.
    Variant,

    /// A field or property of a containing type.
    Field,

    /// A function, method, or lambda.
    Function,

    /// A single value parameter of a function.
    Param,

    /// A generic parameter — a type, lifetime, or const parameter.
    Generic,

    /// A trait, protocol, interface, or typeclass definition.
    Trait,

    /// A concrete `impl` block, inherent or trait.
    Impl,

    /// A named constant or immutable binding.
    Const,

    /// A mutable global or static variable.
    Static,

    /// A type alias / associated type.
    Alias,

    /// A macro definition (declarative, procedural, or preprocessor).
    Macro,

    /// A type expression.
    Type,
}

macro_rules! register_kinds {
	($($(#[$meta:meta])* $kind:ident,)*) => {
		#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
		pub enum Kind {
			$(
			$(#[$meta])*
			$kind($kind),
			)*
		}

		impl Kind {
			#[expect(unused)]
			pub(crate) fn discriminant(&self) -> KindDiscriminant {
				match self {
					$(
					Kind::$kind(_) => KindDiscriminant::$kind,
					)*
				}
			}
		}

		#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
		pub enum KindDiscriminant {
			$(
			$(#[$meta])*
			$kind,
			)*
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
