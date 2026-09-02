//! The direct Clang semantic frontend's in-crate proof tree.
//! These tests exercise the crate-internal analysis terminal because the public fact
//! journey lands with the multi-declaration emission seam; unavailable hosts fail typed.

use super::{
    AnalysisInput, AnalysisScratch, ClangDiagnosticSeverity, ClangError, ClangFact,
    ClangReferenceKind, ClangReport, ClangSourceLanguage, ClangTypeUseResolution, EntityFact,
    MAX_ANALYSIS_SCRATCH_BYTES, SemanticKind, analyze,
};

mod bounds;
mod cancellation;
mod common;
mod kinds;
mod semantic;
