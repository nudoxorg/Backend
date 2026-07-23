use std::any::Any;

use crate::{kinds::*, visitor::Visitor};

register_kinds! {
    /// A namespace, package, or module — a container for other entries.
    Module,

    /// A product type: struct, class, record, or data class.
    Record,

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
