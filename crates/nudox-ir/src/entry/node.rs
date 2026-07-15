use crate::registry::RawEntryIdx;

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Node {
	pub parent:   Option<RawEntryIdx>,
	pub children: Vec<RawEntryIdx>,
}

impl Node {
	pub fn new(parent: RawEntryIdx, children: Vec<RawEntryIdx>) -> Self {
		Self::build(Some(parent), children)
	}

	pub fn root(children: Vec<RawEntryIdx>) -> Self { Self::build(None, children) }

	pub fn leaf(parent: RawEntryIdx) -> Self { Self::build(Some(parent), Vec::new()) }

	pub fn build(parent: Option<RawEntryIdx>, children: Vec<RawEntryIdx>) -> Self {
		Node { parent, children }
	}
}
