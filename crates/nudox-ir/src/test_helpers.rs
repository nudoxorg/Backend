use std::path::PathBuf;

pub(crate) use crate::{
    build::*,
    entry::{EntryInner, Node},
    kind::EntryKind,
    prelude::*,
};

pub(crate) fn id_gen() -> impl FnMut() -> usize {
    let mut next_id = 0usize;

    move || {
        next_id += 1;
        next_id
    }
}

pub(crate) fn sym(name: &str) -> Symbol {
    Symbol {
        name: name.to_owned(),
        visibility: Visibility::Public,
        documentation: String::new(),
        source: PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
    }
}

pub(crate) fn entry<T: EntryKind>(sym: Symbol, node: Node, kind: T) -> Entry {
    Entry::new(sym, node, kind.into_kind())
}

pub(crate) fn n(
    parent: UntypedEntryIndex,
    children: impl IntoIterator<Item = UntypedEntryIndex>,
) -> Node {
    Node::build(parent, children)
}

pub(crate) mod n {
    use super::*;

    pub fn leaf(parent: UntypedEntryIndex) -> Node {
        Node::build(parent, [])
    }

    pub fn root(children: impl IntoIterator<Item = UntypedEntryIndex>) -> Node {
        Node::build(None, children)
    }
}
