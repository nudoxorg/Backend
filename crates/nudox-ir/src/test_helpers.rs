use std::{path::PathBuf, range::Range};

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
	fn resolve_package(&self, _: usize) -> &crate::arena::EntryArena { unimplemented!() }
}
