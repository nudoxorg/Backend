// FIXME: this needs a rethink probably

use std::path::PathBuf;

pub use nudox_ir::{
    entry::{Entry, Node, Symbol, Visibility},
    id::UniqueId,
    index::UntypedEntryIndex,
    kind::EntryKind,
    kinds::*,
    package::{PackageId, PackageMeta},
};

pub fn entry<T>(name: &str, node: Node, kind: T) -> Entry
where
    T: EntryKind,
{
    Entry::new(symbol(name), node, kind.into_kind())
}

pub fn symbol(name: impl Into<String>) -> Symbol {
    Symbol {
        name: name.into(),
        visibility: Visibility::Public,
        documentation: None,
        source: PathBuf::new(),
        span: 0..0,
    }
}

#[allow(unused)]
pub fn n(parent: UntypedEntryIndex, children: impl IntoIterator<Item = UntypedEntryIndex>) -> Node {
    Node::new(parent, children)
}

pub mod n {
    use super::*;

    pub fn root(children: impl IntoIterator<Item = UntypedEntryIndex>) -> Node {
        Node::root(children)
    }

    pub fn leaf(parent: UntypedEntryIndex) -> Node {
        Node::leaf(parent)
    }
}

pub(crate) macro list($($tt:tt)*) {
	vec![$($tt)*].into_boxed_slice()
}
