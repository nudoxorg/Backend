use crate::test_helpers::*;

use super::*;

#[test]
fn serialize_deserialize() {
    let registry = build_registry();

    let dummy_idx = RawEntryIdx::new(10);

    assert_eq!(
        registry
            .state
            .entry_idx_to_unique_id(dummy_idx)
            .downcast_ref::<DummyRegistryResolver>()
            .entry(),
        Some(&8)
    );

    let mut sink = Vec::new();
    let serializer = &mut serde_json::Serializer::new(&mut sink);

    registry
        .serialize(serializer, &dummy_idx)
        .expect("serialization failed");

    let serialized_json = String::from_utf8(sink).expect("wrote invalid utf8 during serialization");

    dbg!(&serialized_json);

    let deserializer = &mut serde_json::Deserializer::from_str(&serialized_json);

    let deserialized_dummy_idx: RawEntryIdx = registry
        .deserialize(deserializer)
        .expect("deserialization failed");

    assert_eq!(dummy_idx, deserialized_dummy_idx);
}

fn build_registry() -> Registry<DummyRegistryResolver> {
    let registry = Registry::default();

    registry.build_package_ir(dummy_package("/pkg-0"), dummy_symbol("pkg-0"), |b| {
        b.create(0, dummy_symbol("mod_1"), |_| Module);
        b.create(1, dummy_symbol("record_2"), |_| Record { fields: list![] });
    });

    registry.build_package_ir(dummy_package("/pkg-1"), dummy_symbol("pkg-1"), |b| {
        b.create(2, dummy_symbol("mod_1"), |_| Module);

        b.create(3, dummy_symbol("record_2"), |_| Record { fields: list![] });

        b.create(4, dummy_symbol("mod_3"), |b| {
            b.create(5, dummy_symbol("mod_4"), |_| Module);
            b.create(6, dummy_symbol("record_5"), |_| Record { fields: list![] });

            Module
        });

        b.create(7, dummy_symbol("record_6"), |b| {
            Record::builder()
                .fields(
                    ["field_7", "field_8", "field_9"]
                        .into_iter()
                        .enumerate()
                        .map(|(idx, sym)| b.create(idx + 8, dummy_symbol(sym), |_| Field {})),
                )
                .build(b)
        });
    });

    registry
}
