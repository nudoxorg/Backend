use super::Node;
use crate::{kind::Kind, symbol::Symbol};

pub struct Entry {
	pub node: Node,
	pub sym:  Symbol,
	pub kind: Kind,
}
