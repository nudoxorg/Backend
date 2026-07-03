//! The canonical symbol record shared by the search and graph layers.
//!
//! This is the *serving* view of a symbol — the minimal, ecosystem-erased shape
//! the read plane speaks — as distinct from the compiler's rich IR entry. It
//! carries the durable [`SymbolId`] so results from tantivy, qdrant, and
//! terminus all join on the same key.

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::{ecosystem::Language, identity::{PackageId, SymbolId}};

// TODO: Merely derive from the IR, no OTHER
#[derive(
	Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, strum::Display, strum::EnumString,
)]
pub enum SymbolKind {
	Module,
	Function,
	Method,
	Record,
	Enum,
	Trait,
	Field,
	Constant,
	TypeAlias,
	Macro,
	/// Anything not captured above; kept so the serving layer never has to fail
	/// on a novel IR kind.
	Other,
}

/// A symbol's identifying names: the bare identifier and its fully-qualified
/// path. Private fields with accessors so the two can't be transposed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Name {
	plain:           SmolStr,
	fully_qualified: SmolStr,
}

impl Name {
	/// Pair a bare identifier with its fully-qualified path.
	pub fn new(plain: impl Into<SmolStr>, fully_qualified: impl Into<SmolStr>) -> Self {
		Self { plain: plain.into(), fully_qualified: fully_qualified.into() }
	}

	/// The bare identifier (e.g. `Router`).
	pub fn plain(&self) -> &str { &self.plain }

	/// The fully-qualified path (e.g. `axum::routing::Router`).
	pub fn fully_qualified(&self) -> &str { &self.fully_qualified }
}

/// The canonical symbol record surfaced by search and graph queries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbol {
	/// The stable, deterministic global identity of this symbol.
	pub id: SymbolId,

	/// The package this symbol belongs to.
	pub package: PackageId,

	/// The ecosystem, carried so the language-erased read plane can still filter.
	pub ecosystem: Language,

	/// The identifying information of the symbol
	pub name: Name,

	/// What kind of thing this symbol is.
	pub kind: SymbolKind,
	// I don't think we need to carry around Generation
}
