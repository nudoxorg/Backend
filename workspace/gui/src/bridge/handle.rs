//! `StreamHandle` — single-ownership cancellation.
//!
//! ## The cancellation policy (read this first)
//!
//! The bridge kernel enforces one cancellation rule and exactly one:
//!
//! **Assigning a new handle to a slot drops and therefore cancels its
//! predecessor.**
//!
//! That's the whole policy. There are no cancel buttons, no explicit
//! `abort()` calls scattered through store methods, no CancellationToken
//! ceremony visible at the call site. A store's query method does:
//!
//! ```text
//! self.slot.handle = Some(new_handle); // old handle dropped here → cancel fires
//! ```
//!
//! Drop invokes the boxed canceller, which forwards to whatever the engine
//! side registered (typically a `tokio_util::CancellationToken::cancel`).
//! The GUI side never sees Tokio types; the canceller is opaque.
//!
//! ## Why single ownership?
//!
//! Multiple owners of a stream handle would mean multiple parties could
//! supersede or cancel a stream independently, making the gen bookkeeping
//! ambiguous. Single ownership (Rust's default) is the correct model here:
//! exactly one slot, exactly one handle, exactly one live query per slot.
//!
//! ## Pairing with `Gen`
//!
//! `StreamHandle` carries the `Gen` that was current when its underlying
//! query was issued. This lets the store verify consistency at construction
//! time (the gen on the handle must equal the gen on the slot) and makes
//! debugging trivial (log the handle and you see the generation).

use crate::bridge::generation::Gen;

/// Ownership token for one live stream.
///
/// # Drop behaviour
///
/// Dropping a `StreamHandle` unconditionally invokes the boxed `canceller`.
/// This fires even on ordinary slot supersession — assigning a new handle to
/// `slot.handle` drops the old one, which cancels the previous query.
///
/// # Note for stores
///
/// You never need to call `cancel()` manually. Store code follows the shape:
///
/// ```text
/// self.slot.generation    = self.gens.next();
/// self.slot.handle = Some(new_handle); // ← old handle cancelled here
/// ```
///
/// and the cancellation happens automatically through `Drop`.
pub struct StreamHandle {
    /// The generation this handle answers. Events whose gen ≠ `slot.generation`
    /// are filtered by the drain closure before they reach store state.
    pub generation: Gen,
    /// Opaque cancellation callback supplied by the engine.
    ///
    /// Called exactly once: when this handle is dropped. The boxed closure
    /// wraps whatever cancellation primitive the engine uses
    /// (e.g. `tokio_util::CancellationToken::cancel`) so the GUI crate
    /// never depends on Tokio types.
    canceller: Box<dyn Fn() + Send>,
}

impl StreamHandle {
    /// Construct a handle for the given generation.
    ///
    /// `canceller` will be called exactly once, when this handle is dropped.
    /// The engine constructs handles; stores receive them.
    pub fn new(generation: Gen, canceller: impl Fn() + Send + 'static) -> Self {
        Self {
            generation,
            canceller: Box::new(canceller),
        }
    }

    /// Explicitly invoke the canceller without waiting for `Drop`.
    ///
    /// This is rarely needed — `Drop` fires the canceller automatically.
    /// The one legitimate use is when a store needs to cancel mid-method
    /// *before* assigning a new handle. Calling this does not prevent `Drop`
    /// from calling the canceller a second time, so the canceller must be
    /// idempotent (token cancellation is idempotent by contract).
    pub fn cancel(&self) {
        (self.canceller)();
    }
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        // This is the entire cancellation mechanism. Nothing else in the GUI
        // crate needs to know a query was abandoned — the engine side detects
        // its token being cancelled and stops producing events.
        (self.canceller)();
    }
}

impl std::fmt::Debug for StreamHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamHandle")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    fn counting_handle(generation: Gen, counter: Arc<AtomicUsize>) -> StreamHandle {
        StreamHandle::new(generation, move || {
            counter.fetch_add(1, Ordering::SeqCst);
        })
    }

    #[test]
    fn drop_invokes_canceller_exactly_once() {
        let counter = Arc::new(AtomicUsize::new(0));
        let handle = counting_handle(Gen(1), counter.clone());
        assert_eq!(counter.load(Ordering::SeqCst), 0, "canceller not yet called");
        drop(handle);
        assert_eq!(counter.load(Ordering::SeqCst), 1, "canceller called exactly once on drop");
    }

    #[test]
    fn cancel_then_drop_calls_canceller_twice() {
        // Cancellers must be idempotent (tokio CancellationToken satisfies this).
        // We document that cancel() + drop() fires twice, so implementations know.
        let counter = Arc::new(AtomicUsize::new(0));
        let handle = counting_handle(Gen(1), counter.clone());
        handle.cancel();
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        drop(handle);
        assert_eq!(counter.load(Ordering::SeqCst), 2, "drop fires canceller independently");
    }

    #[test]
    fn superseding_slot_cancels_predecessor() {
        // This is the primary use-case: assigning a new handle to an Option<StreamHandle>
        // drops the old one, which fires its canceller.
        let counter = Arc::new(AtomicUsize::new(0));

        let mut slot: Option<StreamHandle> = None;

        // First query.
        slot = Some(counting_handle(Gen(1), counter.clone()));
        assert_eq!(counter.load(Ordering::SeqCst), 0);

        // Supersede: old handle dropped, canceller fires.
        slot = Some(counting_handle(Gen(2), counter.clone()));
        assert_eq!(counter.load(Ordering::SeqCst), 1, "predecessor cancelled on supersession");

        // Final drop.
        drop(slot);
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn gen_is_accessible() {
        let handle = StreamHandle::new(Gen(42), || {});
        assert_eq!(handle.generation, Gen(42));
    }
}
