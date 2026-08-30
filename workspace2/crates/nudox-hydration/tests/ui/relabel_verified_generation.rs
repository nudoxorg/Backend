use nudox_hydration::VerifiedGeneration;
use nudox_id::{GenerationId, ObjectDomain};

fn relabel(
    verified: &mut VerifiedGeneration<'_, ObjectDomain, Box<[u8]>>,
    replacement: GenerationId,
) {
    verified.pinned_root = replacement;
}

fn main() {}
