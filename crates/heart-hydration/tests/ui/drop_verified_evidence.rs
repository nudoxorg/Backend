//! Exercises the `heart-hydration` tests ui drop-verified-evidence contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use heart_hydration::VerifiedGeneration;
use backend_version::ObjectDomain;
use backend_store::memory::MemoryStore;

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
