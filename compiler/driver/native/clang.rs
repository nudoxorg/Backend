//! Defines the direct Clang semantic frontend of `compiler-driver`.
//! The frontend binds the exact link-time libclang authority (resolved by the package build
//! script) and produces typed semantic facts — canonical declaration kinds, exact byte-unit
//! spans, typed type uses and references, and structured diagnostics — directly from the
//! caller's source bytes. No process is spawned, no output stream is scanned, and an
//! unavailable link-time authority is a typed failure, never a silent pass.
//!
//! `LIBCLANG_PATH` is the single declared build authority. When the build script cannot
//! bind one exact libclang shared object, this module compiles in its graceful
//! typed-unavailable mode and every analysis returns `ClangError::LibclangUnavailable`.

use std::{path::Path, sync::atomic::AtomicBool, time::Instant};

mod error;
mod protocol;
mod source;

#[cfg(clang_native)]
mod cursor;
#[cfg(clang_native)]
mod ffi;
#[cfg(clang_native)]
mod identity;
#[cfg(clang_native)]
mod parse;
#[cfg(clang_native)]
mod traversal;

#[cfg(test)]
mod tests;

use error::ClangError;
pub use error::ClangFailure;
pub use protocol::{ClangDiagnostic, ClangDiagnosticSeverity, ClangPhase, ClangSourceSpan};
pub(crate) use protocol::{ClangFact, ClangSourceLanguage};

// The remaining fact and kind surface is exercised by this module's proofs and stays
// crate-internal; public fact admission consumes it through the canonical lowering seam.
#[cfg(test)]
pub(crate) use protocol::{ClangReferenceKind, ClangTypeUseResolution, EntityFact, SemanticKind};

/// Exact caller scratch the driver seam reserves for one analysis.
pub(crate) const MAX_ANALYSIS_SCRATCH_BYTES: usize = 256 * 1024;

/// Exact analysis inputs borrowing caller authorities.
pub(crate) struct AnalysisInput<'source, 'input> {
    /// Directory searched for package headers via one `-I` argument.
    pub(crate) include_root: &'input Path,
    /// Exact source name registered for the unsaved buffer; source-relative and free of
    /// absolute checkout paths.
    pub(crate) source_name: &'input Path,
    /// Closed source dialect parsed with the matching `-x` argument.
    pub(crate) source_language: ClangSourceLanguage,
    /// Exact caller source bytes; the only content authority.
    pub(crate) source: &'source [u8],
}

/// Exact caller scratch regions for one analysis.
pub(crate) struct AnalysisScratch<'scratch> {
    /// Identity interner and traversal path region.
    pub(crate) identity: &'scratch mut [u8],
    /// Transactional fact journal region.
    pub(crate) facts: &'scratch mut [u8],
}

/// Work and resource ledger returned by one admitted analysis.
#[allow(
    dead_code,
    reason = "the work and resource ledger is read by this capability's proofs; the multi-declaration emission seam becomes its production consumer at integration"
)]
#[derive(Debug)]
pub(crate) struct ClangReport {
    /// Admitted canonical entity count.
    pub(crate) entities: u32,
    /// Admitted type-use count.
    pub(crate) type_uses: u32,
    /// Admitted reference count.
    pub(crate) references: u32,
    /// Admitted diagnostic count.
    pub(crate) diagnostics: u32,
    /// Exact source byte count.
    pub(crate) source_bytes: u32,
    /// Exact linked-library version byte count observed during the authority check.
    pub(crate) library_version_bytes: usize,
    /// First retained main-file error diagnostic when the source was rejected.
    pub(crate) first_diagnostic: Option<ClangDiagnostic>,
    /// Largest reported per-category native resource amount.
    pub(crate) max_resource_category_bytes: usize,
    /// Aggregate reported native resource amount.
    pub(crate) resource_aggregate_bytes: u128,
    /// Number of nonzero native resource categories.
    pub(crate) resource_categories: usize,
    /// Exact cursor-visit work.
    pub(crate) cursor_visits: usize,
    /// Exact parent-comparison work.
    pub(crate) parent_queries: usize,
    /// Exact identity hash-probe work.
    pub(crate) identity_probes: usize,
    /// Identity scratch high-water mark.
    pub(crate) caller_scratch_high_water: usize,
    /// Fact journal high-water mark.
    pub(crate) fact_scratch_high_water: usize,
    /// Exact journal bytes written.
    pub(crate) journal_used: usize,
}

/// Runs one bounded direct-libclang analysis over the exact caller source bytes.
///
/// Diagnostics stream into the journal first; a rejected translation unit produces a typed
/// rejection and commits no facts. A successful analysis commits every journaled fact
/// through `emit` and returns the work and resource ledger.
pub(crate) fn analyze<'source, 'input>(
    input: AnalysisInput<'source, 'input>,
    expected_version: Option<&'input [u8]>,
    cancellation: Option<&AtomicBool>,
    deadline: Option<Instant>,
    scratch: AnalysisScratch<'_>,
    emit: impl FnMut(ClangFact<'source>),
) -> Result<ClangReport, ClangError<'input>> {
    #[cfg(not(clang_native))]
    #[allow(
        unused_variables,
        reason = "the unavailable terminal is selected before analysis inputs can be consumed"
    )]
    {
        Err(ClangError::LibclangUnavailable)
    }
    #[cfg(clang_native)]
    {
        let mut emit = emit;
        let library_version_bytes = match expected_version {
            Some(expected) => parse::verify_library_version(expected)?,
            None => observed_library_version_bytes(),
        };
        let context = parse::AnalysisContext {
            include_root: input.include_root,
            source_name: input.source_name,
            source_language: input.source_language,
            source: input.source,
            library_version_bytes,
        };
        let AnalysisScratch { identity, facts } = scratch;
        let summary = parse::analyze(context, cancellation, deadline, identity, facts)?;
        if summary.parse_failed {
            return Err(ClangError::ParseRejected {
                source_name: input.source_name,
                diagnostic: summary.first_diagnostic,
            });
        }
        parse::commit_facts(input.source, facts, summary.journal_used, &mut emit)?;
        Ok(ClangReport {
            entities: summary.entities,
            type_uses: summary.type_uses,
            references: summary.references,
            diagnostics: summary.diagnostics,
            source_bytes: u32::try_from(input.source.len()).map_err(|_| {
                ClangError::SourceCoordinateTooLarge {
                    coordinate: input.source.len(),
                }
            })?,
            library_version_bytes: summary.library_version_bytes,
            first_diagnostic: summary.first_diagnostic,
            max_resource_category_bytes: summary.max_resource_category_bytes,
            resource_aggregate_bytes: summary.resource_aggregate_bytes,
            resource_categories: summary.resource_categories,
            cursor_visits: summary.cursor_visits,
            parent_queries: summary.parent_queries,
            identity_probes: summary.identity_probes,
            caller_scratch_high_water: summary.caller_scratch_high_water,
            fact_scratch_high_water: summary.fact_scratch_high_water,
            journal_used: summary.journal_used,
        })
    }
}

/// Observed linked-library version byte count without a family expectation.
#[cfg(clang_native)]
fn observed_library_version_bytes() -> usize {
    use std::ffi::CStr;
    // SAFETY: `clang_getClangVersion` returns a libclang-owned string that remains valid
    // until it is disposed below. No pointer escapes this function.
    let version = unsafe { ffi::clang_get_clang_version() };
    // SAFETY: the string is live for this borrow and disposed at the end of the block.
    let length = {
        let pointer = unsafe { ffi::clang_get_c_string(version) };
        if pointer.is_null() {
            0
        } else {
            // SAFETY: libclang returns a NUL-terminated string for a live handle.
            unsafe { CStr::from_ptr(pointer) }.to_bytes().len()
        }
    };
    // SAFETY: the string is live and is disposed exactly once here.
    unsafe { ffi::clang_dispose_string(version) };
    length
}
