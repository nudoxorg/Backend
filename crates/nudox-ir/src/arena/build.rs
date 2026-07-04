#![expect(unused)]

use super::{Entry, EntryIdx, Node, RawEntryIdx};
use crate::{kind::EntryKind, symbol::Symbol};

pub struct EntryBuilder {
	idx:  usize,
	sym:  Symbol,
	node: Node,
}

impl EntryBuilder {
	pub(super) fn root(idx: usize, sym: Symbol) -> Self {
		EntryBuilder { idx, sym, node: Node { parent: None, children: Vec::new() } }
	}

	pub(super) fn build<T>(mut self, build: impl FnOnce(&mut Self) -> T) -> BuiltEntries
	where
		T: EntryKind,
	{
		let kind = build(&mut self);

		BuiltEntries {
			entry:    Entry { node: self.node, sym: self.sym, kind: kind.into_kind() },
			children: Vec::new(), // TODO
		}
	}
}

pub(super) struct BuiltEntries {
	entry:    Entry,
	children: Vec<BuiltEntries>,
}

impl BuiltEntries {
	pub(super) fn iter(self) -> impl Iterator<Item = Entry> {
		use std::iter::*;

		let first = once(self.entry);

		let rest: Box<dyn Iterator<Item = Entry>> =
			Box::new(self.children.into_iter().flat_map(BuiltEntries::iter));

		chain(first, rest)
	}
}
