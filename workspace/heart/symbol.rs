use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::{
    ecosystem::Language,
    identity::{PackageId, SymbolId},
};

// TODO: Merely derive from the IR, no OTHER and no manualy constructed SYMBOLKIND

/// A symbol's identifying names: the bare identifier and its fully-qualified
/// path. Private fields with accessors so the two can't be transposed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Name {
    pub plain: SmolStr,
    pub fully_qualified: SmolStr,
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

/// What kind of code entity a symbol represents.
///
/// The `Display` token for each variant is its PascalCase name (e.g. `"Function"`).
/// `EnumString` matches the same tokens so `from_str` is the exact inverse of
/// `to_string()` — no manual match table needed.  `VariantNames::VARIANTS` is
/// the static slice the schema CHECK constraint is derived from.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    strum::Display,
    strum::EnumString,
    strum::VariantNames,
)]
pub enum SymbolKind {
    Function,
    Type,
    Module,
    Constant,
    Variable,
    Trait,
    Impl,
    Other,
}
