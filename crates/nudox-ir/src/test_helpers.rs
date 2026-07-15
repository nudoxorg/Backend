use std::path::PathBuf;

use crate::{registry::{EntryArena, RawEntryIdx, RegistryResolver}, symbol::{Symbol, Visibility}};

pub fn dummy_symbol(name: &str) -> Symbol {
	Symbol {
		name:          name.into(),
		visibility:    Visibility::Public,
		documentation: None,
		source:        PathBuf::new(),
		span:          0..0,
	}
}

pub struct DummyRegistry;

impl RegistryResolver for DummyRegistry {
	type EntryId = ();

	fn idx_from_entry_id(&self, _: Self::EntryId) -> RawEntryIdx { unimplemented!() }
	fn entry_id_from_idx(&self, _: RawEntryIdx) -> Self::EntryId { unimplemented!() }
	fn resolve_package(&self, _: usize) -> &EntryArena { unimplemented!() }
}
