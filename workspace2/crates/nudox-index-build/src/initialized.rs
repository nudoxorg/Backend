#![allow(
    unsafe_code,
    reason = "this is the single reviewed MaybeUninit-to-slice proof boundary for caller-owned builder output"
)]

use core::{
    mem::MaybeUninit,
    ops::{Deref, DerefMut},
};

/// A failed exact output initialization, retaining the precise stage and cardinality.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InitializationError<Error> {
    /// Caller output was smaller or larger than the source region selected by preflight.
    Length {
        /// Complete source entries requested by the stage.
        required: usize,
        /// Caller entries supplied to the stage.
        available: usize,
    },
    /// A purported exact-size source ended before every output slot was initialized.
    Exhausted {
        /// Complete output width that was admitted.
        required: usize,
        /// Slots initialized before the source ended.
        initialized: usize,
    },
    /// A purported exact-size source yielded one more entry than admitted output slots.
    Surplus {
        /// Complete output width that was admitted.
        required: usize,
    },
    /// Transforming one source entry returned its exact typed failure.
    Value(Error),
}

/// A borrow proving that every slot in a caller-owned output region is initialized.
pub(crate) struct Initialized<'slots, Value>(&'slots mut [Value]);

impl<'slots, Value> Initialized<'slots, Value> {
    /// Releases the unique construction borrow as a read-only initialized slice.
    pub(crate) const fn into_shared(self) -> &'slots [Value] {
        self.0
    }
}

impl<Value> Deref for Initialized<'_, Value> {
    type Target = [Value];

    fn deref(&self) -> &Self::Target {
        self.0
    }
}

impl<Value> DerefMut for Initialized<'_, Value> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0
    }
}

/// Fills every caller slot exactly once, then lends the initialized slice.
///
/// The source length is checked before the first output write. Its actual exhaustion and surplus
/// are checked before the internal cast, so a lying `ExactSizeIterator` cannot manufacture an
/// initialized borrow.
pub(crate) fn try_initialize<Source, Value: Copy, Error>(
    output: &mut [MaybeUninit<Value>],
    mut source: impl ExactSizeIterator<Item = Source>,
    mut transform: impl FnMut(Source) -> Result<Value, Error>,
) -> Result<Initialized<'_, Value>, InitializationError<Error>> {
    let required = source.len();
    if output.len() != required {
        return Err(InitializationError::Length {
            required,
            available: output.len(),
        });
    }
    for (index, slot) in output.iter_mut().enumerate() {
        let Some(source) = source.next() else {
            return Err(InitializationError::Exhausted {
                required,
                initialized: index,
            });
        };
        slot.write(transform(source).map_err(InitializationError::Value)?);
    }
    if source.next().is_some() {
        return Err(InitializationError::Surplus { required });
    }
    // SAFETY: every output element received `MaybeUninit::write` exactly once in the loop above.
    // The slice pointer and length are preserved. `Value: Copy` forbids a future destructor from
    // making an early-error path leak or observe partially initialized ownership.
    let initialized =
        unsafe { core::slice::from_raw_parts_mut(output.as_mut_ptr().cast(), output.len()) };
    Ok(Initialized(initialized))
}
