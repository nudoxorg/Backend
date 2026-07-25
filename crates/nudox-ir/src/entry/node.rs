use crate::{List, index::RawRef, visitor::Visitor};

#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub(crate) struct Node {
    pub(super) parent: Option<RawRef>,
    pub(super) children: List<RawRef>,
}

impl Node {
    pub(crate) fn build(
        parent: impl Into<Option<RawRef>>,
        children: impl IntoIterator<Item = RawRef>,
    ) -> Self {
        Node {
            parent: parent.into(),
            children: children.into_iter().collect(),
        }
    }
}
