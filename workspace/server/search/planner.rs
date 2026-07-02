//! The search planner — the *only* place a [`SemanticGate`] is minted.
//!
//! The default surface is precise (tantivy) text search. Semantic (qdrant)
//! search is heavy and never implicit: the planner decides — from policy,
//! quota, and the request — whether to engage it, and is the sole issuer of the
//! capability token. Because [`SemanticGate`] is non-`Copy` and consumed by the
//! semantic call, a gate cannot be stashed, forged, or reused; "ran an
//! expensive semantic query by accident" is unrepresentable.

use heart::AccessContext;
use runtime::vector::SemanticGate;

use crate::search::query::{Query, Search};

/// Which surface a request should be routed to, decided by the planner.
pub enum Plan {
	/// Route to precise tantivy search (the default).
	Precise,
	/// Route to semantic search, carrying the minted capability token.
	Semantic(SemanticGate),
}

/// Decides how each request is served and issues the semantic capability when —
/// and only when — the policy permits the heavy path.
pub struct SearchPlanner {
	// TODO: quota / budget / policy handles the planner consults.
}

impl SearchPlanner {
	/// Construct a planner from the server's policy/quota handles.
	pub fn new() -> Self { todo!("wire policy + quota handles") }

	/// Decide the plan for a request. Mints a [`SemanticGate`] (with an audit
	/// reason) iff the request asks for semantic search AND policy/quota allow.
	pub fn plan(&self, request: &Search<'_>, ctx: &AccessContext) -> Plan {
		let _ = ctx;
		match &request.query {
			Query::Literal(_) => Plan::Precise,
			Query::Abstract(_) => {
				// Only here, and only after the (todo) policy/quota check, is a gate issued.
				todo!("check policy+quota; if allowed -> Plan::Semantic(SemanticGate::issue(...))")
			}
		}
	}
}

impl Default for SearchPlanner {
	fn default() -> Self { Self::new() }
}
