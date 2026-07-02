//! The semantic-search gate — a real capability, not a formality.
//!
//! Qdrant-backed semantic search is heavy, so it is NEVER implicit: the default
//! surface is precise (tantivy) text search, and the semantic path runs only
//! when the caller presents a [`SemanticGate`]. The point of the gate is that
//! "ran an expensive semantic query by accident" is unrepresentable.
//!
//! ## Why a capability, not a free `engage()`
//! A gate you can mint for free is not a gate. There is a single constructor,
//! [`SemanticGate::issue`], and the invariant — enforced by convention and the
//! `reason` it demands — is that it is issued **only** by the server's query
//! planner / policy layer (which lives in the `server` crate), after that layer
//! has decided the expensive path is warranted. The `reason` is carried for
//! audit: every semantic query can be traced to *why* it was allowed.
//!
//! The token is:
//! - **non-`Copy` and non-`Clone`** — it cannot be duplicated;
//! - **`#[must_use]`** — obtaining one and dropping it is a warning;
//! - **consumed by value** in the search call — it cannot be stashed and reused
//!   for a second, unaudited query.

/// An explicit, audited opt-in to (heavy) semantic search. Required by every
/// semantic query path; issued only by the planner/policy layer.
///
/// Deliberately not `Copy`/`Clone`: one issuance authorizes one query.
#[must_use = "a SemanticGate authorizes exactly one semantic query; dropping it wastes the authorization"]
#[derive(Debug)]
pub struct SemanticGate {
	/// Why this gate was issued — carried for audit/tracing. Not part of any
	/// equality/identity; purely explanatory.
	reason: &'static str,
}

impl SemanticGate {
	/// Issue a gate authorizing one semantic query, recording *why*.
	///
	/// INVARIANT: call this only from the server's query planner / policy layer,
	/// once it has decided the expensive semantic path is warranted. It is the
	/// deliberate, auditable act of choosing that path. (Because the planner is
	/// in the `server` crate, this is the single sanctioned constructor and the
	/// invariant is documented rather than type-enforced across the crate
	/// boundary; there is intentionally no no-arg `engage`.)
	pub fn issue(reason: &'static str) -> Self {
		Self { reason }
	}

	/// The audit reason this gate was issued for.
	pub const fn reason(&self) -> &'static str { self.reason }
}
