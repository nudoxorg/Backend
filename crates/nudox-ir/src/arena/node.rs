use ecow::EcoVec;

use super::EntryIdx;

pub struct Node {
	pub parent:   Option<EntryIdx<()>>,
	pub children: EcoVec<EntryIdx<()>>,
}
