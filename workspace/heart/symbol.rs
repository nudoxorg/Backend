//! The canonical symbol record shared by the search and graph layers.
//!
//! This is the *serving* view of a symbol — the minimal, ecosystem-erased shape
//! the read plane speaks — as distinct from the compiler's rich IR entry. It
//! carries the durable [`GlobalSymbolId`] so results from tantivy, qdrant, and
//! terminus all join on the same key.

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::{
	content::Generation,
	ecosystem::Ecosystem,
	package::{GlobalSymbolId, PackageId},
};

/// The kind of thing a [`Symbol`] is — the shared, ecosystem-agnostic taxonomy
/// used by search results and the graph layer. (The compiler's IR has a far
/// richer `Entry` kind set; this is the flattened serving projection.)
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

/// The canonical symbol record surfaced by search and graph queries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbol {
	/// The stable, deterministic global identity of this symbol.
	pub id: GlobalSymbolId,

	/// The package this symbol belongs to.
	pub package: PackageId,

	/// The ecosystem, carried so the language-erased read plane can still filter.
	pub ecosystem: Ecosystem,

	/// The bare symbol name (e.g. `Router`).
	pub name: SmolStr,

	/// The fully-qualified name (e.g. `axum::Router`).
	pub fq_name: SmolStr,

	/// What kind of thing this symbol is.
	pub kind: SymbolKind,

	/// The generation (package content hash) this record reflects, so a join
	/// across stores can detect version skew instead of silently mixing.
	pub generation: Generation,
}
