use std::{fmt, hash, marker::PhantomData};

use crate::{kind::EntryKind, registry::DynRegistry};

pub struct EntryIdx<T> {
	package: u32,
	index:   u32,
	_p:      PhantomData<fn() -> T>,
}

impl<T> EntryIdx<T> {
	pub(crate) fn new(package: usize, index: usize) -> Self {
		debug_assert!(u32::try_from(package).is_ok());
		debug_assert!(u32::try_from(index).is_ok());

		EntryIdx { package: package as u32, index: index as u32, _p: PhantomData }
	}

	pub(crate) fn package(self) -> usize { self.package as usize }
	pub(crate) fn index(self) -> usize { self.index as usize }

	pub(crate) fn raw(self) -> RawEntryIdx {
		EntryIdx { package: self.package, index: self.index, _p: PhantomData }
	}

	pub(crate) fn typed<U>(self) -> EntryIdx<U> {
		EntryIdx { package: self.package, index: self.index, _p: PhantomData }
	}
}

impl<T> serde::Serialize for EntryIdx<T> {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: serde::Serializer,
	{
		use serde::ser::Error;

		serde_context::context_scope(|cx| {
			let registry = cx.get::<dyn DynRegistry>().map_err(S::Error::custom)?;

			let value = registry.raw_entry_id_from_idx(self.raw());

			erased_serde::serialize(&value, serializer)
		})
	}
}

impl<'de, T> serde::Deserialize<'de> for EntryIdx<T> {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: serde::Deserializer<'de>,
	{
		use serde::de::Error;

		serde_context::context_scope(|cx| {
			let registry = cx.get::<dyn DynRegistry>().map_err(D::Error::custom)?;

			// TODO: investigate if there's a better way to do this, so that we don't have
			// to use D::Error::custom.
			let idx = registry
				.deser_entry_id_to_idx(&mut <dyn erased_serde::Deserializer>::erase(deserializer))
				.map_err(D::Error::custom)?;

			Ok(idx.typed())
		})
	}
}

impl<T> Clone for EntryIdx<T> {
	fn clone(&self) -> Self { *self }
}

impl<T> Copy for EntryIdx<T> {}

impl<T> fmt::Debug for EntryIdx<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("EntryIdx").field("package", &self.package).field("index", &self.index).finish()
	}
}

impl<T> PartialEq for EntryIdx<T> {
	fn eq(&self, other: &Self) -> bool { self.package == other.package && self.index == other.index }
}

impl<T> Eq for EntryIdx<T> {}

impl<T> hash::Hash for EntryIdx<T> {
	fn hash<H: hash::Hasher>(&self, state: &mut H) {
		self.package.hash(state);
		self.index.hash(state);
	}
}

pub type RawEntryIdx = EntryIdx<private::UntypedMarker>;

// allow converting to a RawEntryIdx from any typed EntryIdx
impl<T: EntryKind> From<EntryIdx<T>> for RawEntryIdx {
	fn from(idx: EntryIdx<T>) -> Self { idx.raw() }
}

mod private {
	pub struct UntypedMarker;
}

#[cfg(test)]
mod tests {
	use bimap::BiMap;

	use super::*;
	use crate::{module::Module, record::{Field, Record}, registry::{EntryArena, Registry}, test_helpers::*};

	#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
	struct ExampleEntryId {
		package: String,
		symbol:  String,
	}

	struct ExamplePackage {
		arena: EntryArena,
	}

	struct ExampleRegistry {
		package_mapping: BiMap<String, usize>,
		packages:        Vec<ExamplePackage>,
	}

	impl Registry for ExampleRegistry {
		type EntryId = ExampleEntryId;

		fn idx_from_entry_id(&self, id: Self::EntryId) -> RawEntryIdx {
			let package_idx =
				*self.package_mapping.get_by_left(&id.package).expect("invalid package identifier");

			let entry_idx = self.packages[package_idx]
				.arena
				.iter()
				.position(|entry| entry.sym.name == id.symbol)
				.expect("invalid symbol identifier");

			EntryIdx::new(package_idx, entry_idx)
		}

		fn entry_id_from_idx(&self, idx: RawEntryIdx) -> Self::EntryId {
			let package =
				self.package_mapping.get_by_right(&idx.package()).expect("invalid package index").clone();
			let entry = self.packages[idx.package()].arena.resolve(idx.index());

			let symbol = entry.sym.name.clone();

			ExampleEntryId { package, symbol }
		}

		fn resolve_package(&self, package_index: usize) -> &EntryArena {
			&self.packages[package_index].arena
		}
	}

	fn build_registry() -> ExampleRegistry {
		let mut registry =
			ExampleRegistry { package_mapping: BiMap::new(), packages: Vec::new() };

		registry.packages.push(ExamplePackage { arena: EntryArena::new(0) });
		registry.package_mapping.insert(String::from("pkg-0"), 0);

		registry.packages[0].arena.create_top_level(dummy_symbol("mod_0"), |b| {
			b.create(dummy_symbol("mod_1"), |_| Module {});
			b.create(dummy_symbol("record_2"), |_| Record { fields: vec![] });

			Module {}
		});

		registry.packages.push(ExamplePackage { arena: EntryArena::new(1) });
		registry.package_mapping.insert(String::from("pkg-1"), 1);

		registry.packages[1].arena.create_top_level(dummy_symbol("mod_0"), |_| Module {});

		registry.packages[1].arena.create_top_level(dummy_symbol("mod_1"), |b| {
			b.create(dummy_symbol("record_2"), |_| Record { fields: vec![] });
			b.create(dummy_symbol("mod_3"), |b| {
				b.create(dummy_symbol("mod_4"), |_| Module {});
				b.create(dummy_symbol("record_5"), |_| Record { fields: vec![] });

				Module {}
			});

			b.create(dummy_symbol("record_6"), |b| Record {
				fields: ["field_7", "field_8", "field_9"]
					.map(dummy_symbol)
					.map(|sym| b.create(sym, |_| Field {}))
					.to_vec(),
			});

			Module {}
		});

		registry
	}

	#[test]
	fn serialize_deserialize() {
		let registry = build_registry();

		let dummy_idx = RawEntryIdx::new(1, 8); // field_8

		assert_eq!(registry.entry_id_from_idx(dummy_idx), ExampleEntryId {
			package: String::from("pkg-1"),
			symbol:  String::from("field_8"),
		});

		let mut sink = Vec::new();
		let serializer = &mut serde_json::Serializer::new(&mut sink);

		serde_context::serialize_with_context(&dummy_idx, serializer, &registry as &dyn DynRegistry)
			.expect("serialization failed");

		let serialized_json = String::from_utf8(sink).expect("wrote invalid utf8 during serialization");

		dbg!(&serialized_json);

		let deserializer = &mut serde_json::Deserializer::from_str(&serialized_json);

		let deserialized_dummy_idx: RawEntryIdx =
			serde_context::deserialize_with_context(deserializer, &registry as &dyn DynRegistry)
				.expect("deserialization failed");

		assert_eq!(dummy_idx, deserialized_dummy_idx);
	}
}
