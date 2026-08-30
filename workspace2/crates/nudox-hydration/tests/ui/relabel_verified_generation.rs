use nudox_hydration::VerifiedGeneration;
use nudox_id::GenerationId;

fn relabel(verified: &mut VerifiedGeneration<'_, ()>, replacement: GenerationId) {
    verified.pinned_root = replacement;
}

fn main() {}
