//! Defines initialized-prefix behavior for `server-runtime`, whose purpose is to schedule bounded work with explicit credits, ownership, and wakeups.
//! This module owns the initialized-prefix invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! One unwind-safe ownership guard for caller-provided `MaybeUninit` arenas.
#![allow(
    unsafe_code,
    reason = "MaybeUninit slice conversion and drop require proving the tracked prefix is initialized"
)]
#![deny(unsafe_op_in_unsafe_fn)]
#![allow(
    clippy::indexing_slicing,
    reason = "len is incremented only after initializing that exact prefix coordinate"
)]

use core::mem::MaybeUninit;

/// Owns a contiguous initialized prefix for the full lifetime of its caller arena.
pub(crate) struct InitializedPrefix<'arena, Element> {
    slots: &'arena mut [MaybeUninit<Element>],
    len: usize,
}

impl<'arena, Element> InitializedPrefix<'arena, Element> {
    /// Initializes the complete arena while retaining exact partial-prefix ownership on unwind.
    pub(crate) fn initialize_with(
        slots: &'arena mut [MaybeUninit<Element>],
        mut build: impl FnMut() -> Element,
    ) -> Self {
        let mut initialized = Self { slots, len: 0 };
        for slot in &mut *initialized.slots {
            slot.write(build());
            initialized.len += 1;
        }
        initialized
    }

    /// Borrows the exact initialized prefix.
    pub(crate) fn as_slice(&self) -> &[Element] {
        // SAFETY: `len` advances only after each successful `MaybeUninit::write`.
        unsafe { self.slots[..self.len].assume_init_ref() }
    }
}

impl<Element> Drop for InitializedPrefix<'_, Element> {
    fn drop(&mut self) {
        // SAFETY: `len` advances only after `write`, so the prefix is exactly the live elements.
        unsafe { self.slots[..self.len].assume_init_drop() };
    }
}
