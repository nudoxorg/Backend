//! Our typestate markers. We of course want to avoid connections or attempted
//! sends to a database that isn't actually alive.
//!
//! This is our attempt to mark this.

use crate::error::ConnectError;

/// A configured-but-unverified store handle.
pub struct Cold;

/// A connected, ready store handle.
pub struct Live;

/// A `Cold` store handle that can verify itself and transition to a `Live`
/// handle of the associated type.
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
