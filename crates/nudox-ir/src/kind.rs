use std::any::Any;

use crate::{module::Module, record::Record, ty::Type};

register_kinds! {
	/// A namespace, package, or module — a container for other entries.
	Module,

	/// A product type: struct, class, record, or data class.
	Record,

	Type,
}

pub trait EntryKind: Any {
	fn into_kind(self) -> Kind
	where
		Self: Sized;
}

macro_rules! register_kinds {
	($($(#[$meta:meta])* $kind:ident,)*) => {
		pub enum Kind {
			$(
			$(#[$meta])*
			$kind($kind),
			)*
		}

		impl Kind {
			pub(crate) fn as_dyn(&self) -> &dyn EntryKind {
				match self {
					$(
					Kind::$kind(it) => it,
					)*
				}
			}
		}

		$(
		impl EntryKind for $kind {
			fn into_kind(self) -> Kind {
				Kind::$kind(self)
			}
		}
		)*
	};
}

use register_kinds;
