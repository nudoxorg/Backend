//! Exercises the `heart-hydration` tests ui relabel-verified-generation contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use heart_hydration::VerifiedGeneration;
use backend_version::{GenerationId, ObjectDomain};

fn relabel(
    verified: &mut VerifiedGeneration<'_, ObjectDomain, Box<[u8]>>,
    replacement: GenerationId,
) {
    verified.pinned_root = replacement;
}

fn main() {}
