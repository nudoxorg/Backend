use crate::{
    List,
    registry::{EntryBuilder, EntryIdx},
};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Record {
    pub fields: List<EntryIdx<Field>>,
}

#[bon::bon]
impl Record {
    #[builder(finish_fn(name = finish, vis = ""))]
    pub fn new(#[builder(with = FromIterator::from_iter)] fields: List<EntryIdx<Field>>) -> Self {
        Record { fields }
    }
}

impl<S: record_builder::IsComplete> RecordBuilder<S> {
    pub fn build(self, b: &mut EntryBuilder) -> Record {
        let record = self.finish();

        b.link_many(record.fields.iter().copied());

        record
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Field {
    // TODO
}

// this serves as an example of the intended usage of the builder APIs.
// note a few key points from the snippets above:
//
// 1. `#[builder(finish_fn(name = finish, vis = ""))]`:
//
// this makes the `finish` fn private, meaning that anyone using the builder
// API can _only_ finish building via a method that _we_ explcitily control
//
// 2. `RecordBuilder::<S>::build`
//
// we expose a public `build` method on the RecordBuilder's completed state,
// allowing the user to instantiate the `Record` by calling our provided method.
//
// this allows us to run the custom code after we call `finish` ourselves, which
// is what enables us to guarentee that we emit IR links if the builder API is
// used.

fn _example_builder_api_usage(b: &mut EntryBuilder) -> Record {
    Record::builder().fields([]).build(b)
}
