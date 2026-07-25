use std::{ops::Range, path::PathBuf};

use crate::visitor::Visitor;

#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum Visibility {
    /// Unrestricted access; visible to all.
    Public,
    /// Visible only within the declaring scope.
    Private,
    /// Visible within the declaring type and its subtypes.
    Protected,
    /// Visible within the assembly or crate (C#-style).
    Internal,
    /// Visible within the package or module (Java/Go unexported).
    Package,
    /// Visible within the current Rust crate (`pub(crate)`).
    Crate,
}

/// A deprecated symbol, optionally carrying a human-readable note and
/// the version in which the deprecation was introduced.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Deprecation {
    /// Human-readable explanation of the deprecation.
    pub note: Option<String>,
    /// Version string in which this deprecation was introduced.
    pub since: Option<String>,
}

/// A cross-reference link embedded in documentation.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct DocLink {
    /// The target symbol or URL that this link points to.
    pub target: String,
    /// Optional display label for the link.
    pub label: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_new_fields_serde_roundtrip() {
        let original = Symbol {
            name: "my_fn".to_owned(),
            visibility: Visibility::Crate,
            documentation: "Does something.".to_owned(),
            source: PathBuf::from("src/lib.rs"),
            span: 10..42,
            aliases: Box::new(["my_function".to_owned(), "myFn".to_owned()]),
            deprecation: Some(Deprecation {
                note: Some("use `new_fn` instead".to_owned()),
                since: Some("1.2.0".to_owned()),
            }),
            doc_links: Box::new([DocLink {
                target: "crate::new_fn".to_owned(),
                label: Some("new_fn".to_owned()),
            }]),
        };

        let json = serde_json::to_string(&original).expect("serialize failed");
        let roundtripped: Symbol = serde_json::from_str(&json).expect("deserialize failed");
        assert_eq!(original, roundtripped);
    }
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
    /// Additional names this symbol is known by (renames/exports).
    pub aliases: crate::List<String>,
    /// Deprecation metadata, if this symbol is deprecated.
    pub deprecation: Option<Deprecation>,
    /// Documentation cross-reference links.
    pub doc_links: crate::List<DocLink>,
}
