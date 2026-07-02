//! The single access-control choke point.
//!
//! Given an [`AccessContext`] (who is asking, in what source scope) and a
//! record's [`Visibility`] + owning [`Tenant`], an [`AccessPolicy`] renders one
//! [`AccessDecision`]. Every backend calls through here; none re-implements it.

use serde::{Deserialize, Serialize};

use super::{source::SourceId, tenant::Principal, tenant::Tenant, visibility::Visibility};

/// The operation being authorized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display)]
pub enum Action {
	/// Reading / searching a record.
	Read,
	/// Publishing / mutating a record.
	Write,
}

/// The scope a request is evaluated in: who is asking and which sources they
/// are querying across. Threaded through every read/write signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessContext {
	/// The authenticated actor.
	pub principal: Principal,
	/// The sources this request is scoped to (empty = the principal's default
	/// federation set).
	pub sources: Vec<SourceId>,
}

/// The outcome of an access check. `Deny` carries a reason for audit/debug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessDecision {
	/// The action is permitted.
	Allow,
	/// The action is refused, with a machine-and-human readable reason.
	Deny { reason: &'static str },
}

impl AccessDecision {
	/// Whether the decision permits the action.
	pub const fn is_allowed(&self) -> bool { matches!(self, AccessDecision::Allow) }
}

/// The policy that renders decisions. One implementation is the system default;
/// enterprises may supply their own.
#[diagnostic::on_unimplemented(
	message = "`{Self}` is not an `AccessPolicy`",
	note = "implement `AccessPolicy` to define who may read/write which records"
)]
pub trait AccessPolicy: Send + Sync {
	/// Decide whether `ctx` may perform `action` on a record with the given
	/// visibility owned by `owner`.
	fn decide(
		&self,
		ctx: &AccessContext,
		action: Action,
		visibility: Visibility,
		owner: Tenant,
	) -> AccessDecision;
}
