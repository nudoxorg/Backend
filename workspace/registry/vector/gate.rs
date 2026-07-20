//! The semantic-search gate — a real capability, not a formality.
//!
//! Qdrant-backed semantic search is heavy, so it is NEVER implicit: the default
//! surface is precise (tantivy) text search, and the semantic path runs only
//! when the caller presents a [`SemanticGate`]. The point of the gate is that
//! "ran an expensive semantic query by accident" is unrepresentable.
//!
//! ## Why a capability, not a free `engage()`
//! A gate you can mint for free is not a gate. User-facing search must go
//! through [`SemanticGate::issue`] via the server's query planner. The one
//! documented exception is [`SemanticGate::for_readiness`]: a fixed-reason
//! constructor for store liveness probes (zero-vector connectivity checks),
//! so audit tooling can distinguish readiness traffic from planner-issued
//! user queries.
//!
//! The token is:
//! - **non-`Copy` and non-`Clone`** — it cannot be duplicated;
//! - **`#[must_use]`** — obtaining one and dropping it is a warning;
//! - **consumed by value** in the search call — it cannot be stashed and reused
//!   for a second, unaudited query.

/// An explicit, audited opt-in to (heavy) semantic search. Required by every
/// semantic query path.
///
/// Deliberately not `Copy`/`Clone`: one issuance authorizes one query.
///
/// **Issuance sites (closed set):**
/// 1. Server query planner — [`SemanticGate::issue`] with a planner reason
/// 2. Readiness probe — [`SemanticGate::for_readiness`] only
#[must_use = "a SemanticGate authorizes exactly one semantic query; dropping it wastes the authorization"]
#[derive(Debug)]
pub struct SemanticGate {
	/// Why this gate was issued — carried for audit/tracing. Not part of any
	/// equality/identity; purely explanatory.
	reason: &'static str,
}

impl SemanticGate {
	/// Issue a gate authorizing one user-facing semantic query, recording *why*.
	///
	/// INVARIANT: call this only from the server's query planner / policy layer,
	/// once it has decided the expensive semantic path is warranted. Readiness
	/// probes must use [`Self::for_readiness`] instead.
	pub fn issue(reason: &'static str) -> Self {
		Self { reason }
	}

	/// Gate for a readiness / liveness probe (not a user query).
	///
	/// The sole sanctioned non-planner issuance site: store `Probeable` impls
	/// that need a one-hit zero-vector connectivity check. Fixed reason so
	/// metrics/audit can filter readiness traffic.
	pub fn for_readiness() -> Self {
		Self {
			reason: "readiness probe",
		}
	}

	/// The audit reason this gate was issued for.
	pub const fn reason(&self) -> &'static str {
		self.reason
	}
}
