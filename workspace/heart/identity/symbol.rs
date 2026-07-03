//! Symbol identity: the in-package locator ([`EntryUri`]) and the deterministic,
//! per-instance [`SymbolId`] derived from it.

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use super::{Id, PackageId, namespace};
use crate::symbol::Symbol;

/// A globally-unique symbol identity — an [`Id`] branded with the serving
/// [`Symbol`] record, so it reads literally as `Id<Symbol>`. Deterministic per
/// graph instance (offline-recomputable) and salted with the TerminusDB instance
/// so two instances of the same corpus don't share ids. Derive one with
/// [`EntryUri::symbol_id`].
pub type SymbolId = Id<Symbol>;

/// A symbol's path *within* a package — the ecosystem-relative locator that,
/// combined with the graph instance, yields a [`SymbolId`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EntryUri {
	/// The package the symbol lives in.
	pub package: PackageId,
	/// The `::`/`.`-agnostic path segments to the symbol within the package.
	pub path:    Box<[SmolStr]>,
}

impl EntryUri {
	/// The canonical string form of this URI.
	pub fn canonical(&self) -> String { todo!("join package id + path segments canonically") }

	/// Derive the deterministic symbol id from the graph-instance token and this
	/// URI — the one construction site. Salting with the instance keeps two
	/// instances of the same corpus from colliding.
	pub fn symbol_id(&self, instance_token: &str) -> SymbolId {
		let mut bytes = instance_token.as_bytes().to_vec();
		bytes.push(0);
		bytes.extend_from_slice(self.canonical().as_bytes());
		Id::from_name(&namespace::SYMBOL, &bytes)
	}
}
