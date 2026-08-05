use std::{ops::Range, path::PathBuf};

use crate::visitor::Visitor;

// ---------------------------------------------------------------------------
// AttrTok
// ---------------------------------------------------------------------------

/// A normalized attribute token on a symbol (§6.2 key 8 / §8.5).
///
/// `token` is an ecosystem-scoped opaque name; `arg` is an optional rendered
/// argument.  Examples:
///
/// - `#[must_use]`        → `AttrTok { token: "must_use", arg: None }`
/// - `#[repr(C)]`         → `AttrTok { token: "repr", arg: Some("C") }`
/// - `#[doc(hidden)]`     → `AttrTok { token: "doc_hidden", arg: None }`
///
/// Unknown or complex attributes are still stored here as opaque tokens so
/// that downstream consumers see *all* attributes, not just the ones the
/// producer understands.
#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct AttrTok {
    /// Attribute name token (e.g. `"must_use"`, `"repr"`, `"doc_hidden"`).
    pub token: String,

    /// Optional argument text (e.g. `"C"` for `repr(C)`).
    pub arg: Option<String>,
}

// ---------------------------------------------------------------------------
// CfgExpr
// ---------------------------------------------------------------------------

/// Normalized cfg predicate guarding a symbol (§8.6).
///
/// Mirrors the recursive boolean structure of `#[cfg(...)]` attributes.
/// Producers normalise the attribute's token-tree into this form so that
/// downstream analysis can reason about conditional compilation without
/// re-parsing raw token streams.
// frozen — never renumber/reorder variants
#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum CfgExpr {
    /// All inner predicates must hold (`cfg(all(...))`).
    All(crate::List<CfgExpr>),

    /// At least one inner predicate must hold (`cfg(any(...))`).
    Any(crate::List<CfgExpr>),

    /// The inner predicate must not hold (`cfg(not(...))`).
    Not(Box<CfgExpr>),

    /// A feature flag (`cfg(feature = "...")`).
    Feature(String),

    /// A target OS predicate (`cfg(target_os = "...")`).
    TargetOs(String),

    /// A target architecture predicate (`cfg(target_arch = "...")`).
    TargetArch(String),

    /// Any other cfg predicate not covered by the variants above.
    ///
    /// Stored as the raw predicate text so that no information is lost.
    Other(String),
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Visitor, serde::Serialize, serde::Deserialize,
)]
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
#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Deprecation {
    /// Human-readable explanation of the deprecation.
    pub note: Option<String>,
    /// Version string in which this deprecation was introduced.
    pub since: Option<String>,
}

/// A cross-reference link embedded in documentation.
#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
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
        // Covers every field of Symbol including attrs and cfg (gap 7).
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
            attrs: Box::new([
                AttrTok {
                    token: "must_use".to_owned(),
                    arg: None,
                },
                AttrTok {
                    token: "repr".to_owned(),
                    arg: Some("C".to_owned()),
                },
            ]),
            cfg: Some(CfgExpr::All(Box::new([
                CfgExpr::Feature("serde".to_owned()),
                CfgExpr::Not(Box::new(CfgExpr::TargetOs("windows".to_owned()))),
                CfgExpr::Any(Box::new([
                    CfgExpr::TargetArch("x86_64".to_owned()),
                    CfgExpr::Other("target_pointer_width = \"64\"".to_owned()),
                ])),
            ]))),
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
#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
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
    /// Normalized attribute tokens on this symbol (§6.2 key 8 / §8.5).
    ///
    /// Empty when the producer does not emit attribute information for this
    /// entry.  Consumers must not treat an empty list as "no attributes" — it
    /// may simply mean the producer did not collect them.
    pub attrs: crate::List<AttrTok>,
    /// Cfg predicate guarding this symbol (§8.6), if any.
    ///
    /// `None` when the symbol is unconditionally compiled in (or when the
    /// producer did not analyse cfg attributes).
    pub cfg: Option<CfgExpr>,
}
