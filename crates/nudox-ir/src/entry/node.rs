use crate::{List, index::RawRef, visitor::Visitor};

/// Structural parent/children edges for one entry.
///
/// In the sealed table the [`crate::apply::PristineIntroTable`] is the
/// authority on structural edges (via `insert_live`'s `parent` argument).
/// Nodes stored inside `Entry` carry whatever edges the builder encoded;
/// after sealing all `Local` refs are lowered to `Intro`/`Foreign` by the
/// visitor pass.
///
/// External consumers that build a `PristineIntroTable` directly should
/// construct a root `Node` with `Node::build(None::<RawRef>, [])` and let
/// the table's `insert_live` call record the parent edge separately.
#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Node {
    pub(super) parent: Option<RawRef>,
    pub(super) children: List<RawRef>,
}

impl Node {
    /// Build a node from a parent reference and an iterator of children.
    ///
    /// Pass `None::<RawRef>` (or any `Into<Option<RawRef>>` yielding `None`)
    /// for a root node with no pre-encoded parent.  Consumers that use
    /// [`crate::apply::PristineIntroTable::insert_live`] to record the parent
    /// edge should pass `None` here; the table is the authority on parent edges
    /// in the sealed representation.
    pub fn build(
        parent: impl Into<Option<RawRef>>,
        children: impl IntoIterator<Item = RawRef>,
    ) -> Self {
        Node {
            parent: parent.into(),
            children: children.into_iter().collect(),
        }
    }
}
