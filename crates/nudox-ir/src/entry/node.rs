use crate::{List, registry::RawEntryIdx};

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Node {
	pub parent:   Option<RawEntryIdx>,
	pub children: List<RawEntryIdx>,
}

impl Node {
	pub fn new(parent: RawEntryIdx, children: impl IntoIterator<Item = RawEntryIdx>) -> Self {
		Self::build(Some(parent), children)
	}

	pub fn root(children: impl IntoIterator<Item = RawEntryIdx>) -> Self {
		Self::build(None, children)
	}

	pub fn leaf(parent: RawEntryIdx) -> Self { Self::build(Some(parent), []) }

	pub fn build(
		parent: Option<RawEntryIdx>,
		children: impl IntoIterator<Item = RawEntryIdx>,
	) -> Self {
		Node { parent, children: children.into_iter().collect() }
	}
}
