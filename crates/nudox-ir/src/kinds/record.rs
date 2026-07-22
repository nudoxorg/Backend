use crate::{List, index::EntryIndex, visitor::Visitor};

#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Record {
    pub fields: List<EntryIndex<Field>>,
}

// impl Visitor for Record {
//     fn visit(&self, f: impl Fn(UntypedEntryIndex)) {
//         self.fields.visit(f)
//     }

//     fn visit_mut(&mut self, f: impl Fn(&mut UntypedEntryIndex)) {
//         self.fields.visit_mut(f)
//     }
// }

// #[bon::bon]
// impl Record {
//     #[builder(finish_fn(name = finish, vis = ""))]
//     pub fn new(#[builder(with = FromIterator::from_iter)] fields:
// List<EntryIdx<Field>>) -> Self {         Record { fields }
//     }
// }

// impl<S: record_builder::IsComplete> RecordBuilder<S> {
//     pub fn build(self, b: &mut EntryBuilder<impl RegistryResolver>) -> Record
// {         let record = self.finish();

//         b.link_many(record.fields.iter().copied());

//         record
//     }
// }

#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Field {
    // TODO
}
