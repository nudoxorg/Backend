use nudox_hydration::VerifiedGeneration;
use nudox_id::ObjectDomain;
use nudox_store_memory::MemoryStore;

fn hypothetical_witness(
    store: &MemoryStore<ObjectDomain>,
) -> VerifiedGeneration<'_, ObjectDomain, Box<[u8]>> {
    let _ = store;
    loop {}
}

fn consume(witness: &VerifiedGeneration<'_, ObjectDomain, Box<[u8]>>) {
    let _ = witness;
}

fn hypothetical_store() -> MemoryStore<ObjectDomain> {
    loop {}
}

fn main() {
    let store = hypothetical_store();
    let witness = hypothetical_witness(&store);
    drop(store);
    consume(&witness);
}
