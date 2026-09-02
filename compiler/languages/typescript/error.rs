//! Defines complete, typed rejection causes at the TypeScript authority boundary.
//! Retains every OXC parser or semantic diagnostic instead of formatting or truncating it.
//! Contains no driver, transport, or canonical-IR policy.

use oxc_diagnostics::Diagnostics;

use crate::Utf8Span;
use thiserror::Error;

/// A TypeScript source could not be admitted to OXC syntax-and-binding analysis.
#[derive(Debug, Error)]
pub enum AuthorityError {
    /// OXC parsed a structurally invalid source and retained every diagnostic.
    #[error("OXC rejected TypeScript syntax")]
    Syntax {
        /// Complete OXC parser diagnostics in source order.
        diagnostics: Diagnostics,
    },
    /// OXC's lexical resolver rejected a parsed source and retained every diagnostic.
    #[error("OXC rejected TypeScript lexical semantics")]
    Binding {
        /// Complete OXC semantic diagnostics in source order.
        diagnostics: Diagnostics,
    },
}

impl AuthorityError {
    /// Returns the first OXC-labelled UTF-8 source range retained by this authority error.
    ///
    /// This is a coordinate witness, not a rendered diagnostic: callers can borrow the exact
    /// corresponding source bytes while retaining this complete OXC error as the source cause.
    #[must_use]
    pub fn primary_span(&self) -> Option<Utf8Span> {
        let diagnostics = match self {
            Self::Syntax { diagnostics } | Self::Binding { diagnostics } => diagnostics,
        };
        let label = diagnostics.first()?.labels.first()?;
        let start = label.offset();
        let length = label.len();
        let end = start.checked_add(length)?;
        Utf8Span::try_from(start..end).ok()
    }
}
