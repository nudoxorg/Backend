mod node;
mod symbol;
mod typed;

use crate::{index::UntypedEntryIndex, kind::Kind, visitor::Visitor};

pub use self::{
    node::Node,
    symbol::{Symbol, Visibility},
    typed::TypedEntry,
};

#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    pub sym: Symbol,
    pub node: Node,
    pub kind: EntryInner,
}

impl Entry {
    pub const fn new(sym: Symbol, node: Node, kind: Kind) -> Self {
        Self {
            sym,
            node,
            kind: EntryInner::Owned(kind),
        }
    }

    pub const fn reference(sym: Symbol, node: Node, idx: UntypedEntryIndex) -> Self {
        Self {
            sym,
            node,
            kind: EntryInner::Reference(idx),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum EntryInner {
    Owned(Kind),
    Reference(UntypedEntryIndex),
}
