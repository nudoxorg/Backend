use nudox_core::GlobalSymbolId;
pub use store::{NUDOX_SYMBOL_NS, symbol_id};

use crate::entry_uri::EntryUri;

/// A Terminus instance identifier in `"{org}/{db}"` form.
///
/// Wrapping the raw string prevents a bare `&str` from silently diverging at
/// call sites — a typo would produce a different [`GlobalSymbolId`] with no
/// compiler warning.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TerminusInstance(String);

impl TerminusInstance {
	pub fn new(s: impl Into<String>) -> Self { Self(s.into()) }

	pub fn as_str(&self) -> &str { &self.0 }
}

impl fmt::Display for TerminusInstance {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
}

use std::fmt;

/// Compute the deterministic [`GlobalSymbolId`] for an [`EntryUri`].
///
/// This is the canonical derivation: `UUIDv5(NUDOX_SYMBOL_NS,
/// "{instance}\0{uri}")`. All stores that need to reference a symbol by ID
/// should call this — never generate random UUIDs for library symbols.
pub fn compute(instance: &TerminusInstance, uri: &EntryUri) -> GlobalSymbolId {
	symbol_id(instance.as_str(), &uri.to_string())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn id_is_deterministic() {
		let instance = TerminusInstance::new("nudox_org/nudox_lib");
		let uri = EntryUri::new("rust", "serde", "serde::Serialize");
		assert_eq!(compute(&instance, &uri), compute(&instance, &uri));
	}

	#[test]
	fn different_uris_give_different_ids() {
		let instance = TerminusInstance::new("nudox_org/nudox_lib");
		let a = compute(&instance, &EntryUri::new("rust", "serde", "serde::Serialize"));
		let b = compute(&instance, &EntryUri::new("rust", "serde", "serde::Deserialize"));
		assert_ne!(a, b);
	}

	#[test]
	fn different_instances_give_different_ids() {
		let uri = EntryUri::new("rust", "serde", "serde::Serialize");
		let a = compute(&TerminusInstance::new("org_a/db_a"), &uri);
		let b = compute(&TerminusInstance::new("org_b/db_b"), &uri);
		assert_ne!(a, b);
	}

	#[test]
	fn matches_store_formula() {
		let instance = TerminusInstance::new("nudox_org/nudox_lib");
		let uri = EntryUri::new("rust", "serde", "serde::Serialize");
		assert_eq!(compute(&instance, &uri), store::symbol_id(instance.as_str(), &uri.to_string()));
	}
}
