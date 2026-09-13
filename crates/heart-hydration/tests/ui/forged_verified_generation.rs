//! Exercises the `heart-hydration` tests ui forged-verified-generation contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use heart_hydration::VerifiedGeneration;
use heart_identity::{GenerationId, ObjectDomain};
use heart_object::DepSetId;

fn forge(pinned_root: GenerationId, dep_set: DepSetId) {
    let _forged: VerifiedGeneration<'static, ObjectDomain, Box<[u8]>> = VerifiedGeneration {
        pinned_root,
        dep_set,
    };
}

fn main() {}
