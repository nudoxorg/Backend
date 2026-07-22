use crate::{List, index::UntypedEntryIndex, visitor::Visitor};

#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Node {
    pub parent: Option<UntypedEntryIndex>,
    pub children: List<UntypedEntryIndex>,
}

impl Node {
    pub fn new(
        parent: UntypedEntryIndex,
        children: impl IntoIterator<Item = UntypedEntryIndex>,
    ) -> Self {
        Self::build(Some(parent), children)
    }

    pub fn root(children: impl IntoIterator<Item = UntypedEntryIndex>) -> Self {
        Self::build(None, children)
    }

    pub fn leaf(parent: UntypedEntryIndex) -> Self {
        Self::build(Some(parent), [])
    }

    pub fn build(
        parent: Option<UntypedEntryIndex>,
        children: impl IntoIterator<Item = UntypedEntryIndex>,
    ) -> Self {
        Node {
            parent,
            children: children.into_iter().collect(),
        }
    }
}
