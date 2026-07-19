use std::{
    convert::Infallible,
    path::{Path, PathBuf},
};

use crate::{
    entry::{Entry, Node},
    kind::EntryKind,
    package::{PackageId, PackageMeta},
    registry::{RawEntryIdx, RegistryResolver, RegistryState, UniqueId},
    symbol::{Symbol, Visibility},
};

pub(crate) use crate::{kinds::*, registry::new_idx as idx};

pub(crate) fn entry<T>(name: &str, node: Node, kind: T) -> Entry
where
    T: EntryKind,
{
    Entry::new(dummy_symbol(name), node, kind.into_kind())
}

pub(crate) fn dummy_symbol(name: impl Into<String>) -> Symbol {
    Symbol {
        name: name.into(),
        visibility: Visibility::Public,
        documentation: None,
        source: PathBuf::new(),
        span: 0..0,
    }
}

pub(crate) fn dummy_package(name: impl AsRef<Path>) -> PackageMeta {
    PackageMeta {
        id: PackageId::path(name),
    }
}

#[expect(unused, reason = "for the future")]
pub(crate) fn n(parent: RawEntryIdx, children: impl IntoIterator<Item = RawEntryIdx>) -> Node {
    Node::new(parent, children)
}

pub(crate) mod n {
    use crate::{entry::Node, registry::RawEntryIdx};

    pub(crate) fn root(children: impl IntoIterator<Item = RawEntryIdx>) -> Node {
        Node::root(children)
    }

    pub(crate) fn leaf(parent: RawEntryIdx) -> Node {
        Node::leaf(parent)
    }
}

#[derive(Default)]
pub(crate) struct DummyRegistryResolver;

impl RegistryResolver for DummyRegistryResolver {
    type EntryId = usize;
    type Error = Infallible;

    async fn load_unique_id(
        &self,
        _: &UniqueId<Self::EntryId>,
        _: &RegistryState<Self>,
    ) -> Result<Entry, Self::Error> {
        unimplemented!()
    }
}

macro_rules! list {
	($($tt:tt)*) => {
		(::std::vec!($($tt)*)).into_boxed_slice()
	};
}

pub(crate) use list;
