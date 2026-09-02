//! Defines complete, typed rejection causes at the TypeScript authority boundary.
//! Retains every OXC parser or semantic diagnostic instead of formatting or truncating it.
//! Contains no driver, transport, or canonical-IR policy.

use oxc_diagnostics::Diagnostics;
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
