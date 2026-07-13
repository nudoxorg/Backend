use crate::idx::RawEntryIdx;

#[derive(Debug, PartialEq, Eq)]
pub struct Node {
	pub parent:   Option<RawEntryIdx>,
	pub children: Vec<RawEntryIdx>,
}
