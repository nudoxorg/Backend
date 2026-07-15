use bimap::BiMap;

use super::*;
use crate::{module::Module, record::{Field, Record}, registry::{EntryArena, RegistryResolver}, test_helpers::*};

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

	serde_context::serialize_with_context(
		&dummy_idx,
		serializer,
		&registry as &dyn DynRegistryResolver,
	)
	.expect("serialization failed");

	let serialized_json = String::from_utf8(sink).expect("wrote invalid utf8 during serialization");

	dbg!(&serialized_json);

	let deserializer = &mut serde_json::Deserializer::from_str(&serialized_json);

	let deserialized_dummy_idx: RawEntryIdx =
		serde_context::deserialize_with_context(deserializer, &registry as &dyn DynRegistryResolver)
			.expect("deserialization failed");

	assert_eq!(dummy_idx, deserialized_dummy_idx);
}

fn build_registry() -> ExampleRegistry {
	let mut registry = ExampleRegistry { package_mapping: BiMap::new(), packages: Vec::new() };

	registry.packages.push(ExamplePackage { arena: EntryArena::new(0) });
	registry.package_mapping.insert(String::from("pkg-0"), 0);

	registry.packages[0].arena.create_top_level(dummy_symbol("mod_0"), |b| {
		b.create(dummy_symbol("mod_1"), |_| Module);
		b.create(dummy_symbol("record_2"), |_| Record { fields: vec![] });

		Module
	});

	registry.packages.push(ExamplePackage { arena: EntryArena::new(1) });
	registry.package_mapping.insert(String::from("pkg-1"), 1);

	registry.packages[1].arena.create_top_level(dummy_symbol("mod_0"), |_| Module);

	registry.packages[1].arena.create_top_level(dummy_symbol("mod_1"), |b| {
		b.create(dummy_symbol("record_2"), |_| Record { fields: vec![] });
		b.create(dummy_symbol("mod_3"), |b| {
			b.create(dummy_symbol("mod_4"), |_| Module);
			b.create(dummy_symbol("record_5"), |_| Record { fields: vec![] });

			Module
		});

		b.create(dummy_symbol("record_6"), |b| Record {
			fields: ["field_7", "field_8", "field_9"]
				.map(dummy_symbol)
				.map(|sym| b.create(sym, |_| Field {}))
				.to_vec(),
		});

		Module
	});

	registry
}

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

impl RegistryResolver for ExampleRegistry {
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
