use crate::{registry::RegistryResolver, test_helpers::*};

use super::*;

#[test]
fn serialize_deserialize() {
	let registry = build_registry();

	let dummy_idx = RawEntryIdx::new(PackageIdx::new(1), ArenaIdx::new(8)); // field_8

	assert_eq!(registry.resolver.idx_to_entry_id(dummy_idx, &registry.state), ExampleEntryId {
		package: PackageId::path("/pkg-0"),
		symbol:  String::from("field_8"),
	});

	let mut sink = Vec::new();
	let serializer = &mut serde_json::Serializer::new(&mut sink);

	registry.serialize(serializer, &dummy_idx).expect("serialization failed");

	let serialized_json = String::from_utf8(sink).expect("wrote invalid utf8 during serialization");

	dbg!(&serialized_json);

	let deserializer = &mut serde_json::Deserializer::from_str(&serialized_json);

	let deserialized_dummy_idx: RawEntryIdx =
		registry.deserialize(deserializer).expect("deserialization failed");

	assert_eq!(dummy_idx, deserialized_dummy_idx);
}

fn build_registry() -> Registry<ExampleRegistryResolver> {
	let mut registry = Registry::new(ExampleRegistryResolver::default());

	registry.build_package_ir(dummy_package("/pkg-0"), dummy_symbol("pkg-0"), |b| {
		b.create(dummy_symbol("mod_1"), |_| Module);
		b.create(dummy_symbol("record_2"), |_| Record { fields: list![] });
	});

	registry.build_package_ir(dummy_package("/pkg-1"), dummy_symbol("pkg-1"), |b| {
		b.create(dummy_symbol("mod_1"), |_| Module);

		b.create(dummy_symbol("record_2"), |_| Record { fields: list![] });

		b.create(dummy_symbol("mod_3"), |b| {
			b.create(dummy_symbol("mod_4"), |_| Module);
			b.create(dummy_symbol("record_5"), |_| Record { fields: list![] });

			Module
		});

		b.create(dummy_symbol("record_6"), |b| {
			Record::builder()
				.fields(
					["field_7", "field_8", "field_9"]
						.map(dummy_symbol)
						.map(|sym| b.create(sym, |_| Field {})),
				)
				.build(b)
		});
	});

	registry
}

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct ExampleEntryId {
	package: PackageId,
	symbol:  String,
}

#[derive(Default)]
struct ExampleRegistryResolver;

impl RegistryResolver for ExampleRegistryResolver {
	type EntryId = ExampleEntryId;

	fn entry_id_to_idx(&self, id: Self::EntryId, state: &RegistryState) -> RawEntryIdx {
		let package = state.resolve_package::<Self>(&id.package, |_| unimplemented!());

		package
			.enumerate()
			.find_map(|(idx, e)| (e.sym.name == id.symbol).then_some(idx))
			.expect("failed to find symbol")
	}

	fn idx_to_entry_id(&self, idx: RawEntryIdx, state: &RegistryState) -> Self::EntryId { todo!() }
}
