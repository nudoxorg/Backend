use std::path::{Path, PathBuf};

use crate::{entry::{Entry, Node}, kind::EntryKind, package::{PackageId, PackageMeta}, registry::{EntryIdx, RawEntryIdx, Registry, RegistryResolver, RegistryState}, symbol::{Symbol, Visibility}};

pub use crate::kinds::*;

pub fn entry<T>(name: &str, node: Node, kind: T) -> Entry
where
	T: EntryKind,
{
	Entry::new(dummy_symbol(name), node, kind.into_kind())
}

pub fn dummy_symbol(name: impl Into<String>) -> Symbol {
	Symbol {
		name:          name.into(),
		visibility:    Visibility::Public,
		documentation: None,
		source:        PathBuf::new(),
		span:          0..0,
	}
}

pub fn dummy_package(name: impl AsRef<Path>) -> PackageMeta {
	PackageMeta { id: PackageId::path(name) }
}

#[expect(unused, reason = "for the future")]
pub fn n(parent: RawEntryIdx, children: impl IntoIterator<Item = RawEntryIdx>) -> Node {
	Node::new(parent, children)
}

pub mod n {
	use crate::{entry::Node, registry::RawEntryIdx};

	pub fn root(children: impl IntoIterator<Item = RawEntryIdx>) -> Node { Node::root(children) }

	pub fn leaf(parent: RawEntryIdx) -> Node { Node::leaf(parent) }
}

pub fn idx<T>(index: usize) -> EntryIdx<T> { crate::registry::new_idx(0, index) }

pub fn dummy_registry() -> Registry<DummyRegistryResolver> { Registry::new(DummyRegistryResolver) }

pub struct DummyRegistryResolver;

impl RegistryResolver for DummyRegistryResolver {
	type EntryId = ();

	fn entry_id_to_idx(&self, _: Self::EntryId, _: &RegistryState) -> RawEntryIdx { unimplemented!() }
	fn idx_to_entry_id(&self, _: RawEntryIdx, _: &RegistryState) -> Self::EntryId { unimplemented!() }
}

macro_rules! list {
	($($tt:tt)*) => {
		(::std::vec!($($tt)*)).into_boxed_slice()
	};
}

pub(crate) use list;
