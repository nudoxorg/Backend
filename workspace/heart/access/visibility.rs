//! Visibility — the ownership dimension every provider is abstracted over, and
//! its subsumption lattice.

use serde::{Deserialize, Serialize};

/// Who a record may be seen by. Ordered by increasing openness; [`Visibility::subsumes`]
/// encodes the lattice (a more-open visibility is visible wherever a
/// less-open one is required).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Visibility {
	/// Visible only to the owning individual principal.
	Personal,
	/// Visible within the owning tenant (e.g. an enterprise's members).
	Private,
	/// Visible to everyone.
	Public,
}

impl Visibility {
	/// Whether a record at `self` visibility is at least as open as `required`
	/// — i.e. anything visible under `required` is also visible under `self`.
	pub const fn subsumes(self, required: Visibility) -> bool { (self as u8) >= (required as u8) }
}
