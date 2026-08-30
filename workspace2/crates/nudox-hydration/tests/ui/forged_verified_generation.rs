use nudox_hydration::VerifiedGeneration;
use nudox_id::{GenerationId, ObjectDomain};
use nudox_object::DepSetId;

fn forge(pinned_root: GenerationId, dep_set: DepSetId) {
    let _forged: VerifiedGeneration<'static, ObjectDomain, Box<[u8]>> = VerifiedGeneration {
        pinned_root,
        dep_set,
    };
}

fn main() {}
