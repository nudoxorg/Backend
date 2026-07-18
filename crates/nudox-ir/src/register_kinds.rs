macro_rules! register_kinds {
	($($(#[$meta:meta])* $mod:ident::$kind:ident,)*) => {
		pub mod kind {
			use std::any::Any;

			use super::kinds::*;

			#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
			pub enum Kind {
				$(
				$(#[$meta])*
				$kind($kind),
				)*
			}

			impl Kind {
				#[expect(unused, reason = "public API helper for a public API that doesn't exist yet")]
				pub(crate) fn variant_as_dyn(&self) -> &dyn Any {
					match self {
						$(
						Kind::$kind(it) => it,
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

		pub mod kinds {
			$(
			pub use $crate::$mod::$kind;
			)*
		}
	};
}

pub(super) use register_kinds;
