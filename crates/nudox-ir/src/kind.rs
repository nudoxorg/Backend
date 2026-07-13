use std::any::Any;

use crate::{module::Module, record::{Field, Record}, ty::Type};

register_kinds! {
	/// A namespace, package, or module — a container for other entries.
	Module,

	/// A product type: struct, class, record, or data class.
	Record,

	Field,

	Type,
}

pub(crate) trait EntryKind: Any {
	fn into_kind(self) -> Kind
	where
		Self: Sized;

	fn discriminant() -> KindDiscriminant
	where
		Self: Sized;
}

macro_rules! register_kinds {
	($($(#[$meta:meta])* $kind:ident,)*) => {
		#[derive(Debug, PartialEq, Eq)]
		pub enum Kind {
			$(
			$(#[$meta])*
			$kind($kind),
			)*
		}

		impl Kind {
			pub(crate) fn variant_as_dyn(&self) -> &dyn EntryKind {
				match self {
					$(
					Kind::$kind(it) => it,
					)*
				}
			}
		}

		#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
		pub(crate) enum KindDiscriminant {
			$(
			$(#[$meta])*
			$kind,
			)*
		}

		$(
		impl EntryKind for $kind {
			fn into_kind(self) -> Kind { Kind::$kind(self) }

			fn discriminant() -> KindDiscriminant { KindDiscriminant::$kind }
		}
		)*
	};
}

use register_kinds;
