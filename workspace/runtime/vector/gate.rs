//! The semantic-search gate.
//!
//! Qdrant-backed semantic search is heavy, so it is NEVER implicit: a caller
//! has to explicitly opt in. The default search surface is precise (tantivy)
//! text search; semantic search only runs when this gate is satisfied.
//!
//! [`SemanticGate`] is the capability token. Its field is private, so the
//! *only* way to obtain one is [`SemanticGate::engage`] — there is no way to
//! reach the semantic path by default, and "ran an expensive semantic query by
//! accident" is unrepresentable.

/// An explicit opt-in to (heavy) semantic search. Required by every semantic
/// query path; impossible to forge.
#[derive(Clone, Copy)]
pub struct SemanticGate(());

impl SemanticGate {
	/// Explicitly engage semantic search. Calling this is the deliberate,
	/// auditable act of choosing the expensive path.
	pub fn engage() -> Self { Self(()) }
}
