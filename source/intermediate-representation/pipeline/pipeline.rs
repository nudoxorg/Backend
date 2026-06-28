use rustc_hash::FxHashMap as HashMap;

use crate::{entry::{Index, NudoxPath}, kind::Entry};

pub struct Collected;
pub struct Indexed;

pub trait Stage {
	type Data;
}

impl Stage for Collected {
	type Data = Vec<Entry>;
}

impl Stage for Indexed {
	type Data = Index;
}

pub struct Ir<S: Stage> {
	data: S::Data,
}

impl Ir<Collected> {
	pub fn from_entries(entries: Vec<Entry>) -> Self { Ir { data: entries } }

	pub fn into_entries(self) -> Vec<Entry> { self.data }

	pub fn index(self) -> Ir<Indexed> {
		let mut entries_by_path =
			HashMap::with_capacity_and_hasher(self.data.len(), Default::default());
		let mut root_ids = Vec::new();

		for entry in self.data {
			if let NudoxPath::Local(p) = entry.path()
				&& p.iter().count() <= 1
			{
				root_ids.push(entry.path().clone());
			}
			entries_by_path.insert(entry.path().clone(), entry);
		}

		Ir { data: Index { root_ids, entries_by_path } }
	}

	pub fn len(&self) -> usize { self.data.len() }

	pub fn is_empty(&self) -> bool { self.data.is_empty() }

	pub fn entries(&self) -> &[Entry] { &self.data }
}

impl Ir<Indexed> {
	pub fn get(&self, id: &NudoxPath) -> Option<&Entry> { self.data.entries_by_path.get(id) }

	pub fn root_ids(&self) -> &[NudoxPath] { &self.data.root_ids }

	pub fn iter(&self) -> impl Iterator<Item = &Entry> { self.data.entries_by_path.values() }

	pub fn len(&self) -> usize { self.data.entries_by_path.len() }

	pub fn is_empty(&self) -> bool { self.data.entries_by_path.is_empty() }

	pub fn into_index(self) -> Index { self.data }
}

impl IntoIterator for Ir<Indexed> {
	type IntoIter = std::collections::hash_map::IntoValues<NudoxPath, Entry>;
	type Item = Entry;

	fn into_iter(self) -> Self::IntoIter { self.data.entries_by_path.into_values() }
}
