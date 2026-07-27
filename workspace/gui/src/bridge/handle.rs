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

/// Ownership token for one live stream.
///
/// This is a **re-export** of `nudox_engine::StreamHandle`, not a parallel type.
///
/// # Why there is only one
///
/// A stream handle's whole job is to cancel engine work when it is dropped.
/// A GUI-side handle wrapping the engine's would be a box around a box: two
/// `Drop` impls and two places for the cancellation contract to be subtly
/// restated. A wrapper whose only content is the thing it wraps adds a name
/// without adding a guarantee — the definition of a leaky abstraction.
///
/// # Drop behaviour
///
/// Dropping a `StreamHandle` invokes its canceller. This fires on ordinary slot
/// supersession too — assigning a new handle to `slot.handle` drops the old
/// one, which cancels the previous query. Store code therefore never calls
/// `cancel()` by hand; it follows the §7.4 shape:
///
/// ```text
/// self.slot.generation = self.gens.next();
/// self.slot.handle     = Some(new_handle); // ← old handle cancelled here
/// ```
///
/// That single ownership rule *is* the cancellation policy (§2.3).
pub use nudox_engine::StreamHandle;
