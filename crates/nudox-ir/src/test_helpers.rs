use std::{convert::Infallible, path::PathBuf, range::Range};

use crate::{registry::Registry, symbol::{NudoxPath, Symbol, Visibility}};

pub fn dummy_symbol(name: &str) -> Symbol {
	Symbol {
		name:          name.into(),
		path:          NudoxPath,
		visibility:    Visibility::Public,
		documentation: None,
		source:        PathBuf::new(),
		span:          Range { start: 0, end: 0 },
	}
}

pub struct DummyRegistry;

impl Registry for DummyRegistry {
	type EntryId = Infallible;

	fn idx_from_entry_id<T>(&self, _: Self::EntryId) -> crate::idx::EntryIdx<T> { unimplemented!() }

	fn resolve_package(&self, _: usize) -> &crate::arena::EntryArena { unimplemented!() }
}
