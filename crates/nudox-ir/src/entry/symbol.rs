use std::{ops::Range, path::PathBuf};

use crate::visitor::Visitor;

#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum Visibility {
    Public,
}

/// Metadata attached to every entry: its name, visibility, documentation, and
/// source location.
///
/// The separation between `Symbol` and the entry's [`Kind`](crate::kind::Kind)
/// is intentional: the same kind data (e.g. a `Record`) can be re-exported
/// under different names, and each re-export carries its own `Symbol`.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Symbol {
    /// The entry's name as written in source code.
    pub name: String,

    /// Whether this name is visible outside its containing package.
    /// `Public` entries are part of the package's public API.
    pub visibility: Visibility,

    /// The raw documentation comment text attached to this item, if any.
    pub documentation: String,

    /// The source file this entry was defined in.
    pub source: PathBuf,

    /// The range of this entry's definition within `source`.
    pub span: Range<usize>,
}
