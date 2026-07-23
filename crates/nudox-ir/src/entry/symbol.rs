use std::{ops::Range, path::PathBuf};

use crate::{List, index::UntypedEntryIndex, kinds::ConstExpr, visitor::Visitor};

/// The visibility of an entry in its source language.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum Visibility {
    /// Unrestricted public access.
    Public,

    /// Crate-level visibility (Rust `pub(crate)`).
    Crate,

    /// Package-level visibility (Java package-private / default).
    Package,

    /// Assembly / module-internal (C# `internal`, Rust `pub(self)`-ish).
    Internal,

    /// Visible to the declaration and its subclasses (`protected`).
    Protected,

    /// Visible only within the declaring scope (`private`).
    Private,

    /// Scoped visibility restricted to a particular module/path
    /// (Rust `pub(in path)`), identified by the module entry it is scoped to.
    Restricted(UntypedEntryIndex),
}

/// A deprecation notice attached to an entry.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Deprecation {
    /// An optional human-readable note (`#[deprecated(note = "...")]`).
    pub note: Option<String>,

    /// The version in which the entry was deprecated (`since = "..."`).
    pub since: Option<String>,
}

/// A conditional-compilation guard controlling whether an entry is present
/// (Rust `#[cfg(...)]`, C `#if`).
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum CfgExpr {
    /// A bare flag (`#[cfg(test)]`, `#[cfg(unix)]`).
    Flag(String),

    /// A key/value option (`target_os = "linux"`, `feature = "serde"`).
    Option { key: String, value: String },

    /// Conjunction (`all(a, b)`).
    All(List<CfgExpr>),

    /// Disjunction (`any(a, b)`).
    Any(List<CfgExpr>),

    /// Negation (`not(a)`).
    Not(Box<CfgExpr>),
}

/// A source-level attribute, annotation, or decorator attached to an entry.
///
/// Covers Rust `#[attr]`, C++ `[[attribute]]`, Java/C# annotations, and
/// Python/TypeScript decorators uniformly.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Attribute {
    /// The attribute name or path (`inline`, `serde`, `Override`, `nodiscard`).
    pub path: String,

    /// Structured arguments, if any (`serde(rename = "x")`, `@Column(name="id")`).
    pub args: List<AttrArg>,
}

/// A single argument to an [`Attribute`].
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum AttrArg {
    /// A positional literal value (`#[repr(C)]` → `C` as a name literal).
    Lit(ConstExpr),

    /// A named / keyword argument (`rename = "x"`).
    Named { name: String, value: ConstExpr },

    /// A nested attribute (`#[derive(Serialize)]`, `cfg(all(a, b))`).
    Nested(Attribute),
}

/// The identifying and documentary metadata common to every entry.
///
/// The `Kind` body carries the entry's *structure*; the `Symbol` carries who it
/// is, where it came from, and how it is documented.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Symbol {
    /// The entry's primary name.
    pub name: String,

    /// The entry's visibility in its source language.
    pub visibility: Visibility,

    /// Rendered documentation prose for the entry. `None` when no doc comment
    /// was written — distinct from an empty one.
    pub documentation: Option<String>,

    /// The conditional-compilation guard under which the entry exists, if any.
    pub cfg: Option<CfgExpr>,

    /// The source file the entry was declared in.
    pub source: PathBuf,

    /// The byte span of the declaration within `source`.
    pub span: Range<usize>,

    /// Alternate names the entry is also known by (re-export aliases, `using`
    /// renames, language-specific synonyms).
    pub aliases: List<String>,

    /// A deprecation notice, if the entry is deprecated.
    pub deprecation: Option<Deprecation>,

    /// Intra-doc links from this entry's documentation to other entries.
    pub doc_links: List<UntypedEntryIndex>,

    /// Source-level attributes / annotations / decorators on the entry.
    pub attributes: List<Attribute>,
}
