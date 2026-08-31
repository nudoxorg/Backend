//! Exercises the `heart-hydration` tests ui escaped-borrowed-root contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use heart_identity::ObjectDomain;
use heart_root::ValidatedRoot;

fn escape(bytes: &[u8]) -> ValidatedRoot<'static, ObjectDomain> {
    ValidatedRoot::try_from(bytes).unwrap()
}
fn main() {}
