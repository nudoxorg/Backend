//! Defines backing behavior for `heart-memory`, whose purpose is to store immutable objects in bounded caller-selected memory.
//! This module owns the backing invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use alloc::{collections::TryReserveError, vec::Vec};
use core::ops::{Deref, DerefMut};

use arrayvec::ArrayVec;

/// Failure to construct one statically selected metadata backing.
#[derive(Debug)]
#[doc(hidden)]
pub enum BackingError {
    /// Exact heap reservation failed at its allocator source.
    Reservation(TryReserveError),
    /// The requested dynamic bound exceeds a compile-time inline policy.
    TooSmall { required: usize, available: usize },
}

/// One initialized-prefix metadata table.
#[doc(hidden)]
pub trait InitializedTable<Element>: Deref<Target = [Element]> + DerefMut {
    /// Appends into the preflighted backing or returns the submitted value unchanged.
    fn try_push(&mut self, value: Element) -> Result<(), Element>;
}

mod sealed {
    pub trait Sealed {}
}

/// Static policy constructing one initialized-prefix metadata table.
#[doc(hidden)]
pub trait MetadataBacking: sealed::Sealed {
    /// Concrete table selected at monomorphization time.
    type Table<Element>: InitializedTable<Element>;

    /// Constructs backing for exactly one declared maximum cardinality.
    fn table<Element>(capacity: usize) -> Result<Self::Table<Element>, BackingError>;

    /// Constructs a table whose usable prefix is initialized to `value`.
    fn filled<Element: Clone>(
        capacity: usize,
        value: Element,
    ) -> Result<Self::Table<Element>, BackingError>;
}

/// Fallibly preallocated heap metadata policy.
pub enum HeapBacking {}

impl sealed::Sealed for HeapBacking {}

impl MetadataBacking for HeapBacking {
    type Table<Element> = HeapTable<Element>;

    fn table<Element>(capacity: usize) -> Result<Self::Table<Element>, BackingError> {
        let mut values = Vec::new();
        values
            .try_reserve_exact(capacity)
            .map_err(BackingError::Reservation)?;
        Ok(HeapTable(values))
    }

    fn filled<Element: Clone>(
        capacity: usize,
        value: Element,
    ) -> Result<Self::Table<Element>, BackingError> {
        let mut table = Self::table(capacity)?;
        table.0.resize(capacity, value);
        Ok(table)
    }
}

/// Metadata policy retaining at most `CAPACITY` elements inside its owner.
pub enum InlineBacking<const CAPACITY: usize> {}

impl<const CAPACITY: usize> sealed::Sealed for InlineBacking<CAPACITY> {}

impl<const CAPACITY: usize> MetadataBacking for InlineBacking<CAPACITY> {
    type Table<Element> = InlineTable<Element, CAPACITY>;

    fn table<Element>(capacity: usize) -> Result<Self::Table<Element>, BackingError> {
        if capacity > CAPACITY {
            return Err(BackingError::TooSmall {
                required: capacity,
                available: CAPACITY,
            });
        }
        Ok(InlineTable(ArrayVec::new()))
    }

    fn filled<Element: Clone>(
        capacity: usize,
        value: Element,
    ) -> Result<Self::Table<Element>, BackingError> {
        if capacity > CAPACITY {
            return Err(BackingError::TooSmall {
                required: capacity,
                available: CAPACITY,
            });
        }
        let mut values = ArrayVec::new();
        for _ in 0..capacity {
            if values.try_push(value.clone()).is_err() {
                return Err(BackingError::TooSmall {
                    required: capacity,
                    available: CAPACITY,
                });
            }
        }
        Ok(InlineTable(values))
    }
}

/// Initialized prefix over one fallibly reserved heap owner.
#[doc(hidden)]
pub struct HeapTable<Element>(Vec<Element>);

impl<Element> Deref for HeapTable<Element> {
    type Target = [Element];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<Element> DerefMut for HeapTable<Element> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<Element> InitializedTable<Element> for HeapTable<Element> {
    fn try_push(&mut self, value: Element) -> Result<(), Element> {
        if self.0.len() == self.0.capacity() {
            Err(value)
        } else {
            self.0.push(value);
            Ok(())
        }
    }
}

/// Initialized prefix over one compile-time inline array.
#[doc(hidden)]
pub struct InlineTable<Element, const CAPACITY: usize>(ArrayVec<Element, CAPACITY>);

impl<Element, const CAPACITY: usize> Deref for InlineTable<Element, CAPACITY> {
    type Target = [Element];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<Element, const CAPACITY: usize> DerefMut for InlineTable<Element, CAPACITY> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<Element, const CAPACITY: usize> InitializedTable<Element> for InlineTable<Element, CAPACITY> {
    fn try_push(&mut self, value: Element) -> Result<(), Element> {
        self.0
            .try_push(value)
            .map_err(arrayvec::CapacityError::element)
    }
}
