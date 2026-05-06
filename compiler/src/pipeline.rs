use std::collections::HashMap;

use gix::ObjectId;
use ir::{entry::{Index, NudoxPath}, kind::Entry};
use semver::Version;

/// Entries freshly collected from a language parser — unindexed and
/// unversioned.
pub struct Collected;

/// Entries with that conform to a standard of versioning
pub struct Versioned;

/// Entries indexed by ID, cross-referenceable, ready for emission.
pub struct Indexed;

/// Associates each pipeline stage with its internal data representation.
pub trait Stage {
	type Data;
}

impl Stage for Collected {
	type Data = Vec<Entry>;
}

/// A trait for grounding an external object to the git model
#[allow(dead_code)]
struct GroundedEntry {
	commit:  ObjectId,
	branch:  String,
	version: Version,
	object:  Entry,
}

impl Stage for Versioned {
	type Data = Index;
}

impl Stage for Indexed {
	type Data = Index;
}

pub struct Ir<S: Stage> {
	data: S::Data,
}

#[allow(dead_code)]
impl Ir<Collected> {
	/// Wrap raw parser output into the IR pipeline.
	pub fn from_entries(entries: Vec<Entry>) -> Self { Ir { data: entries } }

	/// Consume the collected IR and return the underlying entries.
	pub fn into_entries(self) -> Vec<Entry> { self.data }

	/// Build the ID index, transitioning to the `Indexed` stage.
	///
	/// Root entries are those whose path has a single segment (the crate root).
	pub fn index(self) -> Ir<Indexed> {
		let mut entries_by_path = HashMap::with_capacity(self.data.len());
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

	/// Number of collected entries.
	pub fn len(&self) -> usize { self.data.len() }

	pub fn is_empty(&self) -> bool { self.data.is_empty() }

	/// Borrow the raw entry list.
	pub fn entries(&self) -> &[Entry] { &self.data }
}

#[allow(dead_code)]
impl Ir<Indexed> {
	/// Look up an entry by its ID.
	pub fn get(&self, id: &NudoxPath) -> Option<&Entry> { self.data.entries_by_path.get(id) }

	/// The root entry IDs.
	pub fn root_ids(&self) -> &[NudoxPath] { &self.data.root_ids }

	/// Iterate over all indexed entries (by reference).
	pub fn iter(&self) -> impl Iterator<Item = &Entry> { self.data.entries_by_path.values() }

	/// Number of indexed entries.
	pub fn len(&self) -> usize { self.data.entries_by_path.len() }

	pub fn is_empty(&self) -> bool { self.data.entries_by_path.is_empty() }

	/// Consume the IR and return the underlying `Index`.
	pub fn into_index(self) -> Index { self.data }
}

/// Allows `Ir<Indexed>` to be fed directly to anything accepting
/// `IntoIterator<Item = Entry>` (e.g. `Runner::run`).
impl IntoIterator for Ir<Indexed> {
	type IntoIter = std::collections::hash_map::IntoValues<NudoxPath, Entry>;
	type Item = Entry;

	fn into_iter(self) -> Self::IntoIter { self.data.entries_by_path.into_values() }
}

#[allow(dead_code)]
impl Ir<Versioned> {
	/// Look up an entry by its ID.
	pub fn get(&self, id: &NudoxPath) -> Option<&Entry> { self.data.entries_by_path.get(id) }

	/// The root entry IDs.
	pub fn root_ids(&self) -> &[NudoxPath] { &self.data.root_ids }

	/// Iterate over all versioned entries (by reference).
	pub fn iter(&self) -> impl Iterator<Item = &Entry> { self.data.entries_by_path.values() }

	/// Number of versioned entries.
	pub fn len(&self) -> usize { self.data.entries_by_path.len() }

	pub fn is_empty(&self) -> bool { self.data.entries_by_path.is_empty() }

	/// Consume the IR and return the underlying `Index`.
	pub fn into_index(self) -> Index { self.data }
}

/// Allows `Ir<Versioned>` to be fed directly to anything accepting
/// `IntoIterator<Item = Entry>` (e.g. `Runner::run`).
impl IntoIterator for Ir<Versioned> {
	type IntoIter = std::collections::hash_map::IntoValues<NudoxPath, Entry>;
	type Item = Entry;

	fn into_iter(self) -> Self::IntoIter { self.data.entries_by_path.into_values() }
}
