// FIXME: this needs a rethink probably

use std::path::PathBuf;

pub use nudox_ir::{
    entry::{Entry, Node},
    kind::EntryKind,
    kinds::*,
    package::{PackageId, PackageMeta},
    registry::{RawEntryIdx, UniqueId},
    symbol::{Symbol, Visibility},
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
pub fn n(parent: RawEntryIdx, children: impl IntoIterator<Item = RawEntryIdx>) -> Node {
    Node::new(parent, children)
}

pub mod n {
    use super::*;

    pub fn root(children: impl IntoIterator<Item = RawEntryIdx>) -> Node {
        Node::root(children)
    }

    pub fn leaf(parent: RawEntryIdx) -> Node {
        Node::leaf(parent)
    }
}

macro_rules! list {
	($($tt:tt)*) => {
		(::std::vec!($($tt)*)).into_boxed_slice()
	};
}

pub(crate) use list;
