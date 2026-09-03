//! Defines caller-owned, bounded storage supplied to one libclang collection attempt.
//! Every native fact is written into one provided typed slot array or returns a capacity error.
//! No semantic fact vector, string arena, or background cache is allocated by this crate.

use crate::facts::{
    DeclarationFact, DiagnosticFact, IncludeFact, OverrideFact, ReferenceFact, TypeEdge, TypeFact,
};

/// Typed slot arrays reserved by a caller for one collection transaction.
#[derive(Debug)]
pub struct ClangScratch<'scratch> {
    /// Destination slots for direct declaration facts.
    pub declarations: &'scratch mut [DeclarationFact],
    /// Destination slots for direct recursive type facts.
    pub types: &'scratch mut [TypeFact],
    /// Destination slots for recursive type edges.
    pub type_edges: &'scratch mut [TypeEdge],
    /// Destination slots for direct reference facts.
    pub references: &'scratch mut [ReferenceFact],
    /// Destination slots for structured diagnostics.
    pub diagnostics: &'scratch mut [DiagnosticFact],
    /// Destination slots for include authority facts.
    pub includes: &'scratch mut [IncludeFact],
    /// Destination slots for C++ override-authority facts.
    pub overrides: &'scratch mut [OverrideFact],
}
