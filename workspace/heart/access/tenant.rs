//! The owner a source or record belongs to, and the principal every access
//! decision is evaluated for.

use serde::{Deserialize, Serialize};

use crate::identifier::Id;

/// The unit that owns records and against which access is granted: an
/// individual user or an enterprise organization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tenant {
	/// A single user account.
	Individual(Id<Tenant>),
	/// An enterprise/organization account (may have many member principals).
	Enterprise(Id<Tenant>),
}

impl Tenant {
	/// The raw tenant id, regardless of kind.
	pub const fn id(&self) -> Id<Tenant> {
		match self {
			Tenant::Individual(id) | Tenant::Enterprise(id) => *id,
		}
	}
}

/// An authenticated actor making a request. Carries the tenant it acts on
/// behalf of; richer claims (roles, scopes) attach here later without touching
/// call sites.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Principal {
	/// The principal's own stable id.
	pub id: Id<Principal>,
	/// The tenant this principal is acting as.
	pub tenant: Tenant,
}
