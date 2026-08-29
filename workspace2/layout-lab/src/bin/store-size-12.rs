fn main() {
    let store = nudox_store_memory::InlineMemoryStore::<nudox_id::ObjectDomain, 1, 2>::new(
        nudox_store_memory::StoreCapacity {
            bytes: 0_u64.into(),
            slots: 0_u32.into(),
        },
    );
    let _value = std::hint::black_box(store);
}
