mod node;
mod typed;

use crate::{kind::Kind, registry::RawEntryIdx, symbol::Symbol};

pub use self::{node::Node, typed::TypedEntry};

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

    pub const fn reference(sym: Symbol, node: Node, idx: RawEntryIdx) -> Self {
        Self {
            sym,
            node,
            kind: EntryInner::Reference(idx),
        }
    }
}

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EntryInner {
    Owned(Kind),
    Reference(RawEntryIdx),
}
