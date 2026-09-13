//! Exercises the `heart-hydration` tests ui forged-validated-root contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use backend_version::ObjectDomain;
use heart_root::{RootEntryCount, ValidatedLocality, ValidatedRoot};

fn main() {
}

fn rewrite(root: &mut ValidatedRoot<'_, ObjectDomain>) {
    root.entry_count = RootEntryCount::from(1);
}

fn rewrite_locality(locality: &mut ValidatedLocality<'_, ObjectDomain>) {
    locality.root_count = RootEntryCount::from(1);
}
