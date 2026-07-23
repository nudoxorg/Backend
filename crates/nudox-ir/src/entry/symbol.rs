use std::{ops::Range, path::PathBuf};

use crate::{List, index::UntypedEntryIndex, visitor::Visitor};

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

    /// Rendered documentation prose for the entry.
    pub documentation: String,

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
}
