use crate::{List, index::UntypedEntryIndex, visitor::Visitor};

#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub(crate) struct Node {
    pub(super) parent: Option<UntypedEntryIndex>,
    pub(super) children: List<UntypedEntryIndex>,
}

impl Node {
    pub(crate) fn build(
        parent: impl Into<Option<UntypedEntryIndex>>,
        children: impl IntoIterator<Item = UntypedEntryIndex>,
    ) -> Self {
        Node {
            parent: parent.into(),
            children: children.into_iter().collect(),
        }
    }
}
