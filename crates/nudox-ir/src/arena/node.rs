use ecow::EcoVec;

use super::EntryIdx;

#[derive(Debug, PartialEq, Eq)]
pub struct Node {
	pub parent:   Option<EntryIdx<()>>,
	pub children: EcoVec<EntryIdx<()>>,
}
