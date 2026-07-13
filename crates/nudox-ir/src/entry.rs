mod node;
mod typed;

pub use self::{node::Node, typed::TypedEntry};
use crate::{kind::Kind, symbol::Symbol};

#[derive(Debug, PartialEq, Eq)]
pub struct Entry {
	pub node: Node,
	pub sym:  Symbol,
	pub kind: Kind,
}
