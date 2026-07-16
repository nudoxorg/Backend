#![feature(trait_alias)]

pub mod entry;
pub mod function;
pub mod kind;
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

#[cfg(test)]
mod test_helpers;
// pub mod generics;
// pub mod parameter;
// pub mod protocols;
// pub mod syntax;
