//! Connection typestate markers + the shared connect protocol.
//!
//! A store handle is either [`Cold`] (configured, but not yet verified to be
//! reachable/migrated) or [`Live`] (connected and ready). Query methods live
//! only on the `Live` form, and the `Server` is assemblable only from `Live`
//! stores — so "used a store before it was connected" is a compile error.
//!
//! [`Connect`] unifies the three hand-rolled `connect()` methods into one
//! protocol so `Server::assemble` can `try_join!` every backend through a
//! single, uniform path with one error type.

use crate::error::ConnectError;

/// A configured-but-unverified store handle.
pub struct Cold;

/// A connected, ready store handle.
pub struct Live;

/// A `Cold` store handle that can verify itself and transition to a `Live`
/// handle of the associated type. Implemented by every backing store, so the
/// server assembles them uniformly.
#[diagnostic::on_unimplemented(
	message = "`{Self}` cannot be connected",
	note = "implement `Connect` so the server can bring this store up uniformly"
)]
pub trait Connect: Sized {
	/// The `Live` handle produced on success (query methods live there).
	type Live;

	/// Verify reachability/credentials/schema, then promote to `Live`.
	async fn connect(self) -> Result<Self::Live, ConnectError>;
}
