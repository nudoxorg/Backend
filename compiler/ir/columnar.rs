//! One-allocation backing for independently typed structure-of-arrays lanes.
//!
//! This is the only unsafe storage primitive in semantic IR. It exists because
//! stable Rust's `Vec<T>` cannot share an allocator block across element types.
//! The public IR never exposes the owner or raw pointers: consumers receive
//! ordinary slices whose lifetimes remain tied to the containing column family.

#![allow(
    unsafe_code,
    reason = "audited typed SoA slab; invariants are documented at each unsafe operation"
)]

use alloc::alloc::{alloc, dealloc, handle_alloc_error};
use core::{
    alloc::Layout,
    marker::PhantomData,
    ptr::NonNull,
    slice,
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT_PLAN_ID: AtomicUsize = AtomicUsize::new(1);

fn next_plan_id() -> usize {
    NEXT_PLAN_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .expect("column slab plan identity exhausted")
}

/// Offset and capacity calculated before the shared allocation exists.
pub(crate) struct ColumnSpec<T: Copy> {
    plan_id: usize,
    offset: usize,
    capacity: usize,
    element: PhantomData<T>,
}

/// Incrementally calculates a valid combined `Layout` for typed lanes.
pub(crate) struct SlabPlan {
    id: usize,
    layout: Layout,
}

impl Default for SlabPlan {
    fn default() -> Self {
        Self {
            id: next_plan_id(),
            layout: Layout::new::<()>(),
        }
    }
}

impl SlabPlan {
    pub(crate) fn column<T: Copy>(&mut self, capacity: usize) -> ColumnSpec<T> {
        assert_ne!(size_of::<T>(), 0, "columnar lanes must not be zero-sized");
        let lane = Layout::array::<T>(capacity).expect("column capacity exceeds address space");
        let (layout, offset) = self
            .layout
            .extend(lane)
            .expect("combined column slab exceeds address space");
        self.layout = layout;
        ColumnSpec {
            plan_id: self.id,
            offset,
            capacity,
            element: PhantomData,
        }
    }

    pub(crate) fn allocate(self) -> Slab {
        Slab::new(self.id, self.layout.pad_to_align())
    }
}

/// Sole allocation owner. Typed pointers never outlive this value.
pub(crate) struct Slab {
    plan_id: usize,
    allocation: NonNull<u8>,
    layout: Layout,
}

impl Slab {
    fn new(plan_id: usize, layout: Layout) -> Self {
        if layout.size() == 0 {
            return Self {
                plan_id,
                allocation: NonNull::dangling(),
                layout,
            };
        }
        // SAFETY: `layout` came from `Layout::extend`/`pad_to_align` and is
        // nonzero. Null is handled with the standard allocation failure path.
        let allocation = unsafe { alloc(layout) };
        let Some(allocation) = NonNull::new(allocation) else {
            handle_alloc_error(layout);
        };
        Self {
            plan_id,
            allocation,
            layout,
        }
    }

    pub(crate) fn bind<T: Copy>(&self, spec: ColumnSpec<T>) -> RawColumn<T> {
        assert_eq!(
            spec.plan_id, self.plan_id,
            "column spec belongs to another slab"
        );
        let byte_len = size_of::<T>()
            .checked_mul(spec.capacity)
            .expect("column byte length exceeds address space");
        assert!(
            spec.offset
                .checked_add(byte_len)
                .is_some_and(|end| end <= self.layout.size()),
            "column spec exceeds its slab"
        );
        if spec.capacity == 0 {
            return RawColumn {
                pointer: NonNull::dangling(),
                len: 0,
                capacity: 0,
                element: PhantomData,
            };
        }
        // SAFETY: `spec.offset` was produced by extending this slab's plan with
        // `Layout::array::<T>`, so the address is in-bounds and T-aligned. No
        // reference is created until an element has been initialized.
        let pointer = unsafe { self.allocation.as_ptr().add(spec.offset).cast::<T>() };
        RawColumn {
            pointer: NonNull::new(pointer).expect("slab base is non-null"),
            len: 0,
            capacity: spec.capacity,
            element: PhantomData,
        }
    }
}

impl Drop for Slab {
    fn drop(&mut self) {
        if self.layout.size() != 0 {
            // SAFETY: this is the exact pointer/layout pair returned by
            // `alloc`, and Slab is its unique owner.
            unsafe { dealloc(self.allocation.as_ptr(), self.layout) };
        }
    }
}

/// Typed initialized prefix inside a [`Slab`].
pub(crate) struct RawColumn<T: Copy> {
    pointer: NonNull<T>,
    len: usize,
    capacity: usize,
    element: PhantomData<T>,
}

impl<T: Copy> RawColumn<T> {
    pub(crate) fn push(&mut self, value: T) {
        assert!(self.len < self.capacity, "column reservation invariant");
        // SAFETY: `len < capacity`; the slot belongs to this column and is
        // written before the initialized prefix is extended.
        unsafe { self.pointer.as_ptr().add(self.len).write(value) };
        self.len += 1;
    }

    pub(crate) fn extend_from_slice(&mut self, values: &[T]) {
        assert!(values.len() <= self.capacity.saturating_sub(self.len));
        // SAFETY: source and destination cannot overlap (the destination is a
        // newly allocated slab during growth), and the checked range fits.
        unsafe {
            self.pointer
                .as_ptr()
                .add(self.len)
                .copy_from_nonoverlapping(values.as_ptr(), values.len());
        }
        self.len += values.len();
    }

    pub(crate) fn as_slice(&self) -> &[T] {
        // SAFETY: exactly the initialized prefix is exposed, and its lifetime
        // cannot outlive the column family that owns the sibling Slab.
        unsafe { slice::from_raw_parts(self.pointer.as_ptr(), self.len) }
    }

    pub(crate) fn as_mut_slice(&mut self) -> &mut [T] {
        // SAFETY: `&mut self` uniquely borrows this column descriptor and each
        // typed lane is disjoint by construction of `SlabPlan`.
        unsafe { slice::from_raw_parts_mut(self.pointer.as_ptr(), self.len) }
    }

    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }
}

impl<T: Copy> core::ops::Deref for RawColumn<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_alignment_initialized_prefixes_and_owner_move_are_sound() {
        let mut plan = SlabPlan::default();
        let bytes = plan.column::<u8>(3);
        let words = plan.column::<u64>(2);
        let slab = plan.allocate();
        let mut bytes = slab.bind(bytes);
        let mut words = slab.bind(words);
        bytes.push(7);
        bytes.push(9);
        words.push(0xfeed_beef_dead_cafe);

        let moved_owner = slab;
        assert_eq!(bytes.as_slice(), &[7, 9]);
        assert_eq!(words.as_slice(), &[0xfeed_beef_dead_cafe]);
        assert_eq!((words.as_slice().as_ptr() as usize) % align_of::<u64>(), 0);
        drop(moved_owner);
    }

    #[test]
    fn copying_into_a_fresh_lane_preserves_exact_values() {
        let mut first_plan = SlabPlan::default();
        let first_spec = first_plan.column::<u32>(2);
        let first_slab = first_plan.allocate();
        let mut first = first_slab.bind(first_spec);
        first.push(11);
        first.push(13);

        let mut next_plan = SlabPlan::default();
        let next_spec = next_plan.column::<u32>(4);
        let next_slab = next_plan.allocate();
        let mut next = next_slab.bind(next_spec);
        next.extend_from_slice(first.as_slice());
        next.push(17);
        assert_eq!(next.as_slice(), &[11, 13, 17]);
    }
}
