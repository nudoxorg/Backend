//! The lines your workspace uses a declaration on (W-Open I3): what a
//! declaration page needs to paint its "In your workspace" list on the very
//! first frame, with no wait for a file read.
//!
//! The index names a use by a relation and a byte span. The line at that span
//! is read from your own file, off the UI thread, **as part of reading the
//! page** (`runtime::workspace_lines`), so a page never lands without its
//! lines, and the launch snapshot, which saves the page, saves them too. A
//! place whose file cannot be read is left out: a line is never guessed.

use backend_library::SemanticLinkKind;
use std::sync::Arc;

/// Whether the index resolved a use, or matched it by name.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Resolution {
    /// The compiler, an import or the index resolved it to the declaration.
    Resolved,
    /// A name matched: the use may be another declaration of the same name.
    ByName,
}

/// One place your workspace uses the declaration, with its line.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UseLine {
    /// The package it is in (its display name).
    pub package: Arc<str>,
    /// The file, relative to the package.
    pub file: Arc<str>,
    /// The file's absolute path, for opening it in the editor.
    pub path: Arc<str>,
    /// The line, one-based.
    pub line: u32,
    /// The line's text, trimmed and cut at [`Self::MAX_TEXT`] characters.
    pub text: Arc<str>,
    /// What the index says the relation is.
    pub relation: SemanticLinkKind,
    /// Whether the index resolved it.
    pub resolution: Resolution,
}

impl UseLine {
    /// The longest line kept (a minified file's one line is not a sentence).
    pub const MAX_TEXT: usize = 240;
}
