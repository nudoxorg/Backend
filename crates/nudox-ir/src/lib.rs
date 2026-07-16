#![feature(trait_alias)]

pub mod entry;
pub mod function;
pub mod package;
pub mod primitive;
pub mod record;
pub mod registry;
pub mod symbol;
pub mod ty;

pub mod module {
	#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
	pub struct Module;
}

pub type List<T> = Box<[T]>;

register_kinds::register_kinds! {
	/// A namespace, package, or module — a container for other entries.
	module::Module,

	/// A product type: struct, class, record, or data class.
	record::Record,

	/// A field or property of a containing type.
	record::Field,

	function::Function,

	ty::Type,
}

mod register_kinds;

#[cfg(test)]
mod test_helpers;
// pub mod generics;
// pub mod parameter;
// pub mod protocols;
// pub mod syntax;
