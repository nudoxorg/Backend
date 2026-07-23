mod node;
mod symbol;
mod typed;

use crate::{index::UntypedEntryIndex, kind::Kind, visitor::Visitor};

pub(crate) use self::node::Node;

pub use self::{
    symbol::{AttrArg, Attribute, CfgExpr, Deprecation, Symbol, Visibility},
    typed::TypedEntry,
};

#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    sym: Symbol,
    node: Node,
    kind: EntryInner,
}

impl Entry {
    pub fn sym(&self) -> &Symbol {
        &self.sym
    }

    pub fn parent(&self) -> Option<UntypedEntryIndex> {
        self.node.parent
    }

    pub fn children(&self) -> &[UntypedEntryIndex] {
        &self.node.children
    }

    pub fn kind(&self) -> &EntryInner {
        &self.kind
    }

    pub(crate) const fn new(sym: Symbol, node: Node, kind: Kind) -> Self {
        Self {
            sym,
            node,
            kind: EntryInner::Owned(kind),
        }
    }

    pub(crate) const fn reference(sym: Symbol, node: Node, idx: UntypedEntryIndex) -> Self {
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
