use std::any::Any;

use crate::{kinds::*, visitor::Visitor};

// FIXME(deferred-kinds): the following Kinds from the old `ir/` tree are not yet
// ported, deliberately, to keep this pass focused:
//   - Trait / Impl  — depend on the generics + constraints subsystem, which is
//                      intentionally cut for now (see the generics FIXMEs).
//   - Const / Static — trivial to add once the const-expression subsystem for
//                      their initializers exists.
//   - Alias (TypeAlias), Macro, Event — pending a decision on whether each earns
//                      a distinct Kind or folds into an existing one.
// Re-exports (old `Reexport`) are already modelled by `EntryInner::Reference`,
// not a Kind.
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

    /// A single parameter of a function.
    Param,

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
