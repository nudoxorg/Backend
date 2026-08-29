use nudox_hydration::VerifiedGeneration;
use nudox_id::GenerationId;
use nudox_object::DepSetId;

fn forge(pinned_root: GenerationId, dep_set: DepSetId) {
    let _forged = VerifiedGeneration {
        pinned_root,
        dep_set,
    };
}

fn main() {}
