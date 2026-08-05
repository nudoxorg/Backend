mod node;
mod symbol;
mod typed;

use crate::{index::RawRef, kind::Kind, visitor::Visitor};

pub use self::node::Node;

pub use self::{
    symbol::{AttrTok, CfgExpr, Deprecation, DocLink, Symbol, Visibility},
    typed::TypedEntry,
};

/// A single node in the package IR tree.
///
/// Every item in a package---modules, structs, enums, functions, fields,
/// parameters---is represented as an `Entry`. Entries form a tree via
/// parent/child links stored internally.
///
/// # Structure
///
/// An `Entry` bundles three things:
///
/// * **Symbol** — the entry's name, visibility, doc comment, and source
///   location. Accessed via [`Entry::sym`].
/// * **Node** — parent and child indices that define the tree. Accessed via
///   [`Entry::parent`] and [`Entry::children`].
/// * **Kind** — the semantic payload. Accessed via [`Entry::kind`]. This can be
///   either an *owned* [`Kind`] variant (e.g. `Kind::Record(…)`) or a
///   *reference* to another entry, representing a re-export or type alias.
///
/// # Tree structure
///
/// ```text
/// Module "my_pkg"
///   ├── Record "Point"
///   │   ├── Field "x"
///   │   └── Field "y"
///   └── Function "distance"
///       └── Param "other"
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    sym: Symbol,
    node: Node,
    kind: EntryInner,
}

impl Entry {
    pub fn sym(&self) -> &Symbol {
        &self.sym
    }

    pub fn parent(&self) -> Option<&RawRef> {
        self.node.parent.as_ref()
    }

    pub fn children(&self) -> &[RawRef] {
        &self.node.children
    }

    pub fn kind(&self) -> &EntryInner {
        &self.kind
    }

    /// Construct an owned entry from its symbol, node, and kind.
    ///
    /// The `node` encodes the parent/children structural edges. For tests that
    /// build a [`crate::apply::PristineIntroTable`] directly (bypassing the
    /// `seal` pass), use [`crate::apply::PristineIntroTable::insert_live`] to
    /// record the parent edge separately and pass a `Node` built from
    /// `Node::build(None::<RawRef>, [])` — the table is the authority on
    /// structural edges in the sealed representation.
    pub fn new(sym: Symbol, node: Node, kind: Kind) -> Self {
        Self {
            sym,
            node,
            kind: EntryInner::Owned(kind),
        }
    }

    pub fn reference(sym: Symbol, node: Node, idx: RawRef) -> Self {
        Self {
            sym,
            node,
            kind: EntryInner::Reference(idx),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum EntryInner {
    Owned(Kind),
    Reference(RawRef),
}

impl EntryInner {
    /// Returns the inner [`Kind`] if this is an `Owned` entry, else `None`.
    ///
    /// Use this to match on the specific kind variant after retrieving an
    /// entry from an [`crate::apply::PristineIntroTable`] or [`crate::view::IrView`].
    pub fn as_owned_kind(&self) -> Option<&Kind> {
        match self {
            EntryInner::Owned(k) => Some(k),
            EntryInner::Reference(_) => None,
        }
    }

    /// Returns the [`KindDiscriminant`] of the owned kind, or `None` for a
    /// reference entry.
    pub fn discriminant(&self) -> Option<crate::kind::KindDiscriminant> {
        match self {
            EntryInner::Owned(k) => Some(k.discriminant()),
            EntryInner::Reference(_) => None,
        }
    }
}
