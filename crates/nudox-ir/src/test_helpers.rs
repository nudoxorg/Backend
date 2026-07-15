use std::path::PathBuf;

use crate::{entry::{Entry, Node}, kind::EntryKind, registry::{ArenaIdx, EntryArena, EntryIdx, PackageIdx, RawEntryIdx, RegistryResolver}, symbol::{Symbol, Visibility}};

pub use crate::{module::Module, record::{Field, Record}};

pub fn entry<T>(name: &str, node: Node, kind: T) -> Entry
where
	T: EntryKind,
{
	Entry::new(dummy_symbol(name), node, kind.into_kind())
}

pub fn dummy_symbol(name: &str) -> Symbol {
	Symbol {
		name:          name.into(),
		visibility:    Visibility::Public,
		documentation: None,
		source:        PathBuf::new(),
		span:          0..0,
	}
}

#[expect(unused, reason = "for the future")]
pub fn n(parent: RawEntryIdx, children: Vec<RawEntryIdx>) -> Node { Node::new(parent, children) }

pub mod n {
	use crate::{entry::Node, registry::RawEntryIdx};

	pub fn root(children: Vec<RawEntryIdx>) -> Node { Node::root(children) }

	pub fn leaf(parent: RawEntryIdx) -> Node { Node::leaf(parent) }
}

pub fn idx<T>(index: usize) -> EntryIdx<T> {
	EntryIdx::new(PackageIdx::new(0), ArenaIdx::new(index))
}

pub struct DummyRegistry;

impl RegistryResolver for DummyRegistry {
	type EntryId = ();

	fn idx_from_entry_id(&self, _: Self::EntryId) -> RawEntryIdx { unimplemented!() }
	fn entry_id_from_idx(&self, _: RawEntryIdx) -> Self::EntryId { unimplemented!() }
	fn resolve_package(&self, _: usize) -> &EntryArena { unimplemented!() }
}
