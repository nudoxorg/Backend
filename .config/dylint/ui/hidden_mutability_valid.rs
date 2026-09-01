//! Exercises explicit atomic coordination without a diagnostic.
//! Keeps the value public while the synchronization primitive remains typed.
//! Proves atomics are not grouped with hidden mutable containers.
use std::sync::atomic::AtomicUsize;

pub struct AtomicCounter {
    pub value: AtomicUsize,
}

fn main() {}
