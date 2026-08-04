//! Named query files and their result row structs.
//!
//! Each `.trustfall` file in this directory is a named query that can be
//! executed against a [`crate::adapter::CorpusAdapter`].  The result row
//! structs use [`serde::Deserialize`] so that
//! [`trustfall::TryIntoStruct`] can deserialize query output rows into
//! typed values.
//!
//! # Field naming
//!
//! Trustfall output field names come from `@output(name: "...")` or the bare
//! property name.  GraphQL property names are camelCase (`isAsync`), so the
//! Rust fields use `#[serde(rename = "...")]` to bridge the gap.

use serde::Deserialize;

// ---------------------------------------------------------------------------
// Named query sources
// ---------------------------------------------------------------------------

/// Source text of the `find_symbol_by_key` query.
pub const FIND_SYMBOL_BY_KEY: &str =
    include_str!("find_symbol_by_key.trustfall");

/// Source text of the `list_package_functions` query.
pub const LIST_PACKAGE_FUNCTIONS: &str =
    include_str!("list_package_functions.trustfall");

/// Source text of the `find_usages` query.
pub const FIND_USAGES: &str =
    include_str!("find_usages.trustfall");

/// Source text of the `find_implementors` query.
pub const FIND_IMPLEMENTORS: &str =
    include_str!("find_implementors.trustfall");

/// Source text of the `symbols_mentioning_type` query.
pub const SYMBOLS_MENTIONING_TYPE: &str =
    include_str!("symbols_mentioning_type.trustfall");

// ---------------------------------------------------------------------------
// Result row structs
// ---------------------------------------------------------------------------

/// A row produced by [`FIND_SYMBOL_BY_KEY`].
#[derive(Debug, Deserialize)]
pub struct FindSymbolByKeyRow {
    pub key: String,
    pub name: String,
    pub kind: String,
    pub path: Option<String>,
    pub documentation: String,
}

/// A row produced by [`LIST_PACKAGE_FUNCTIONS`].
#[derive(Debug, Deserialize)]
pub struct ListPackageFunctionsRow {
    pub key: String,
    pub name: String,
    pub path: Option<String>,
    pub documentation: String,
    #[serde(rename = "isAsync")]
    pub is_async: Option<bool>,
    #[serde(rename = "receiverKind")]
    pub receiver_kind: Option<String>,
}

/// A row produced by [`FIND_USAGES`].
#[derive(Debug, Deserialize)]
pub struct FindUsagesRow {
    pub key: String,
    pub name: String,
    pub kind: String,
}

/// A row produced by [`FIND_IMPLEMENTORS`].
#[derive(Debug, Deserialize)]
pub struct FindImplementorsRow {
    pub key: String,
    pub name: String,
}

/// A row produced by [`SYMBOLS_MENTIONING_TYPE`].
#[derive(Debug, Deserialize)]
pub struct SymbolsMentioningTypeRow {
    pub key: String,
    pub name: String,
    pub kind: String,
}
