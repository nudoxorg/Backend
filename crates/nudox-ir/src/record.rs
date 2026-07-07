use crate::arena::EntryIdx;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
	pub fields: Vec<EntryIdx<Field>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
	// TODO
}
