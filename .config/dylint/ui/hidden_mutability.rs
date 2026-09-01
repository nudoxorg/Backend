//! Exercises resolved mutex rejection beside an atomic protocol neighbor.
//! Uses fully-qualified paths so import spelling cannot affect the test.
//! Freezes the public hidden-coordination diagnostic.
use std::sync::{Mutex, atomic::AtomicUsize};

pub struct AtomicCounter {
    pub value: AtomicUsize,
}

pub struct LockedCounter {
    pub value: Mutex<usize>,
}

fn main() {}
