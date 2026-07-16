mod node;
mod typed;

pub use self::{node::Node, typed::TypedEntry};
use crate::{kind::Kind, registry::RawEntryIdx, symbol::Symbol};

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    pub sym: Symbol,
    pub node: Node,
    pub kind: EntryInner,
}
impl Entry {
    pub(crate) const fn new(sym: Symbol, node: Node, kind: Kind) -> Self {
        Self {
            sym,
            node,
            kind: EntryInner::Owned(kind),
        }
    }

    #[expect(unused)] // TODO: test and actually use
    pub(crate) const fn reference(sym: Symbol, node: Node, idx: RawEntryIdx) -> Self {
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
