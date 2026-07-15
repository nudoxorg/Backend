use crate::registry::RawEntryIdx;

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Node {
	pub parent:   Option<RawEntryIdx>,
	pub children: Vec<RawEntryIdx>,
}
