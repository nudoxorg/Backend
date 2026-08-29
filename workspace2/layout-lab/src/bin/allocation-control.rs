//! Independent `allocation-counter` control for store construction/drop.

use nudox_store_memory::{MemoryStore, StoreCapacity};

fn main() {
    println!("format\tnudox-layout-allocation-counter-v1");
    println!(
        "columns\tslots\tcount_total\tcount_current\tcount_max\tbytes_total\tbytes_current\tbytes_max"
    );
    for slots in [0_u32, 1, 4, 5, 100_000] {
        let facts = allocation_counter::measure(|| {
            let store = MemoryStore::<nudox_id::ObjectDomain>::new(StoreCapacity {
                bytes: u64::from(slots).saturating_mul(8).into(),
                slots: slots.into(),
            });
            drop(store);
        });
        println!(
            "{slots}\t{}\t{}\t{}\t{}\t{}\t{}",
            facts.count_total,
            facts.count_current,
            facts.count_max,
            facts.bytes_total,
            facts.bytes_current,
            facts.bytes_max,
        );
    }
}
