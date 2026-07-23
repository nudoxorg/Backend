use crate::{List, index::EntryIndex, visitor::Visitor};

#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Record {
    pub fields: List<EntryIndex<Field>>,
}

#[bon::bon]
impl Record {
    #[builder]
    pub fn new(#[builder(with = FromIterator::from_iter)] fields: List<EntryIndex<Field>>) -> Self {
        Record { fields }
    }
}

#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Field {
    // TODO
}
