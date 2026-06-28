use nudox_core::GlobalSymbolId;
pub use store::{NUDOX_SYMBOL_NS, symbol_id};

use crate::entry_uri::EntryUri;

/// Compute the deterministic [`GlobalSymbolId`] for an [`EntryUri`].
///
/// This is the canonical derivation: `UUIDv5(NUDOX_SYMBOL_NS, "{instance}\0{uri}")`.
/// All stores that need to reference a symbol by ID should call this — never
/// generate random UUIDs for library symbols.
pub fn compute(terminus_instance: &str, uri: &EntryUri) -> GlobalSymbolId {
	symbol_id(terminus_instance, &uri.to_string())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn id_is_deterministic() {
		let instance = "nudox_org/nudox_lib";
		let uri = EntryUri::new("rust", "serde", "serde::Serialize");
		assert_eq!(compute(instance, &uri), compute(instance, &uri));
	}

	#[test]
	fn different_uris_give_different_ids() {
		let instance = "nudox_org/nudox_lib";
		let a = compute(instance, &EntryUri::new("rust", "serde", "serde::Serialize"));
		let b = compute(instance, &EntryUri::new("rust", "serde", "serde::Deserialize"));
		assert_ne!(a, b);
	}

	#[test]
	fn different_instances_give_different_ids() {
		let uri = EntryUri::new("rust", "serde", "serde::Serialize");
		let a = compute("org_a/db_a", &uri);
		let b = compute("org_b/db_b", &uri);
		assert_ne!(a, b);
	}

	#[test]
	fn matches_store_formula() {
		let instance = "nudox_org/nudox_lib";
		let uri = EntryUri::new("rust", "serde", "serde::Serialize");
		assert_eq!(compute(instance, &uri), store::symbol_id(instance, &uri.to_string()));
	}
}
