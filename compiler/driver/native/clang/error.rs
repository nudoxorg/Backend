//! Defines error behavior for the direct Clang semantic frontend of `compiler-driver`.
//! Every terminal retains its exact rejected value or a precise observed lens; no cause is
//! erased into a catch-all. The borrowed error type keeps analysis failures allocation-free,
//! and its total owned mirror crosses the driver terminal boundary without borrowing caller
//! paths that outlive no invocation.

use std::path::{Path, PathBuf};

use super::protocol::{ClangDiagnostic, ClangPhase};
use thiserror::Error;

/// Exact typed failure of one direct Clang analysis, borrowing caller paths.
#[derive(Debug, Error)]
pub(crate) enum ClangError<'input> {
    /// The linked libclang authority is absent from this build.
    #[cfg(not(clang_native))]
    #[error("the linked libclang authority is unavailable in this build")]
    LibclangUnavailable,
    /// The linked library version family differs from the expected version bytes.
    #[error(
        "linked libclang version family does not match the {expected_bytes}-byte expected version, observed lens {observed} bytes"
    )]
    ToolVersionMismatch {
        /// Exact expected version byte count retained from the caller declaration.
        expected_bytes: usize,
        /// Observed library version byte count.
        observed: usize,
    },
    /// libclang could not create its translation-unit index.
    #[error("could not create the libclang index")]
    LibclangIndexCreate,
    /// An analysis argument could not be represented as a NUL-free C string.
    #[error("could not represent the Clang analysis arguments as C strings")]
    LibclangArgumentNul,
    /// The source bytes contain an interior NUL that the unsaved buffer cannot carry.
    #[error("source bytes contain an interior NUL")]
    LibclangSourceNul,
    /// libclang rejected the parse call itself with its exact status code.
    #[error("libclang could not parse {source_name:?} (code {code})")]
    LibclangParse {
        /// Exact source name bound to the unsaved buffer.
        source_name: &'input Path,
        /// Exact libclang status code.
        code: u32,
    },
    /// libclang could not derive the unified symbol resolution identity of a declaration.
    #[error("libclang could not derive the unified symbol identity of a declaration")]
    LibclangIdentityUnavailable,
    /// A referenced declaration resolves outside the analyzed source.
    #[error("a declaration referenced from {source_name:?} lives outside the analyzed source")]
    LibclangExternalIdentityUnavailable {
        /// Exact source name bound to the unsaved buffer.
        source_name: &'input Path,
    },
    /// The traversal callback observed a Rust panic and stopped before crossing into C.
    #[error("the libclang traversal callback panicked")]
    LibclangCallbackPanicked,
    /// The traversal lost its ancestor path and refused to invent an owner.
    #[error("libclang traversal lost its ancestor path")]
    LibclangTraversalParentUnavailable,
    /// The caller fact journal rejected the exact capacity requirement.
    #[error(
        "libclang rejected {source_name:?}: fact scratch holds {provided} bytes, requires {required}"
    )]
    LibclangFactScratchTooSmall {
        /// Exact source name bound to the unsaved buffer.
        source_name: &'input Path,
        /// Caller-provided fact scratch bytes.
        provided: usize,
        /// Exact required fact scratch bytes.
        required: usize,
    },
    /// The caller identity scratch rejected the exact capacity requirement.
    #[error(
        "libclang rejected {source_name:?}: identity scratch holds {provided} bytes, requires {required}"
    )]
    LibclangIdentityScratchTooSmall {
        /// Exact source name bound to the unsaved buffer.
        source_name: &'input Path,
        /// Caller-provided identity scratch bytes.
        provided: usize,
        /// Exact required identity scratch bytes.
        required: usize,
    },
    /// libclang tokenization exceeded the caller-derived token capacity.
    #[error(
        "libclang rejected {source_name:?}: tokenization observed {observed} tokens above the {limit} token capacity"
    )]
    LibclangTokenCapacity {
        /// Exact source name bound to the unsaved buffer.
        source_name: &'input Path,
        /// Observed token count.
        observed: usize,
        /// Caller-derived token capacity.
        limit: usize,
    },
    /// The native authority rejected the source with its first main-file error diagnostic.
    #[error("libclang rejected {source_name:?}")]
    ParseRejected {
        /// Exact source name bound to the unsaved buffer.
        source_name: &'input Path,
        /// First retained main-file error diagnostic when one exists.
        diagnostic: Option<ClangDiagnostic>,
    },
    /// Cancellation was observed at the named analysis phase.
    #[error("Clang analysis was cancelled during {phase:?}")]
    Cancelled {
        /// Exact phase observing the cancellation.
        phase: ClangPhase,
    },
    /// The deadline was exceeded at the named analysis phase.
    #[error("Clang analysis exceeded its deadline during {phase:?}")]
    DeadlineExceeded {
        /// Exact phase observing the deadline.
        phase: ClangPhase,
    },
    /// A source coordinate exceeded the addressable source extent.
    #[error("source coordinate {coordinate} exceeds the addressable source extent")]
    SourceCoordinateTooLarge {
        /// Exact rejected coordinate.
        coordinate: usize,
    },
    /// A source column exceeded the retained coordinate width.
    #[error("source column {column} on line {line} exceeds the coordinate width")]
    SourceCoordinateColumnTooLarge {
        /// Exact rejected line.
        line: u32,
        /// Exact rejected column.
        column: u32,
    },
    /// The diagnostic location had no representable line or column coordinate.
    #[error("source coordinate line {line} column {column} is unavailable")]
    SourceCoordinateUnavailable {
        /// Exact rejected line.
        line: u32,
        /// Exact rejected column.
        column: u32,
    },
    /// The diagnostic location fell outside the exact source bytes.
    #[error(
        "source coordinate line {line} column {column} is outside the {source_bytes}-byte source"
    )]
    SourceCoordinateOutOfBounds {
        /// Exact rejected line.
        line: u32,
        /// Exact rejected column.
        column: u32,
        /// Exact source byte count.
        source_bytes: usize,
    },
    /// Declaration nesting exceeded the caller traversal capacity.
    #[error("declaration nesting reaches depth {depth} above the traversal capacity")]
    AstNestingTooDeep {
        /// Exact rejected depth.
        depth: usize,
    },
    /// The interner rejected one more identity than the caller capacity admits.
    #[error("identity binding capacity {limit} was exceeded at {observed} entries")]
    BindingCapacity {
        /// Exact admitted caller capacity.
        limit: usize,
        /// Exact observed entry count including the rejected one.
        observed: usize,
    },
    /// A checked fact counter exceeded its address space.
    #[error("fact count exceeded the counter address space")]
    FactCountOverflow,
    /// A fact journal record written by this analysis did not decode; an internal invariant broke.
    #[error("fact journal record {ordinal} did not decode")]
    JournalRecordInvalid {
        /// Exact ordinal of the rejected journal record.
        ordinal: usize,
    },
}

/// Owned, lifetime-free mirror of every [`ClangError`] cause, carried by the driver terminal.
///
/// Borrowed path and version facts are retained as exact lengths or owned paths; no cause is
/// dropped. The conversion is only reachable on failure paths, so the allocation is cold.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ClangFailure {
    /// The linked libclang authority is absent from this build.
    #[error("the linked libclang authority is unavailable in this build")]
    LibclangUnavailable,
    /// The linked library version family differs from the expected version bytes.
    #[error(
        "linked libclang version family does not match the {expected_bytes}-byte expected version, observed lens {observed} bytes"
    )]
    ToolVersionMismatch {
        /// Exact expected version byte count retained from the caller declaration.
        expected_bytes: usize,
        /// Observed library version byte count.
        observed: usize,
    },
    /// libclang could not create its translation-unit index.
    #[error("could not create the libclang index")]
    LibclangIndexCreate,
    /// An analysis argument could not be represented as a NUL-free C string.
    #[error("could not represent the Clang analysis arguments as C strings")]
    LibclangArgumentNul,
    /// The source bytes contain an interior NUL that the unsaved buffer cannot carry.
    #[error("source bytes contain an interior NUL")]
    LibclangSourceNul,
    /// libclang rejected the parse call itself with its exact status code.
    #[error("libclang could not parse {source_name:?} (code {code})")]
    LibclangParse {
        /// Exact source name bound to the unsaved buffer.
        source_name: PathBuf,
        /// Exact libclang status code.
        code: u32,
    },
    /// libclang could not derive the unified symbol resolution identity of a declaration.
    #[error("libclang could not derive the unified symbol identity of a declaration")]
    LibclangIdentityUnavailable,
    /// A referenced declaration resolves outside the analyzed source.
    #[error("a declaration referenced from {source_name:?} lives outside the analyzed source")]
    LibclangExternalIdentityUnavailable {
        /// Exact source name bound to the unsaved buffer.
        source_name: PathBuf,
    },
    /// The traversal callback observed a Rust panic and stopped before crossing into C.
    #[error("the libclang traversal callback panicked")]
    LibclangCallbackPanicked,
    /// The traversal lost its ancestor path and refused to invent an owner.
    #[error("libclang traversal lost its ancestor path")]
    LibclangTraversalParentUnavailable,
    /// The caller fact journal rejected the exact capacity requirement.
    #[error(
        "libclang rejected {source_name:?}: fact scratch holds {provided} bytes, requires {required}"
    )]
    LibclangFactScratchTooSmall {
        /// Exact source name bound to the unsaved buffer.
        source_name: PathBuf,
        /// Caller-provided fact scratch bytes.
        provided: usize,
        /// Exact required fact scratch bytes.
        required: usize,
    },
    /// The caller identity scratch rejected the exact capacity requirement.
    #[error(
        "libclang rejected {source_name:?}: identity scratch holds {provided} bytes, requires {required}"
    )]
    LibclangIdentityScratchTooSmall {
        /// Exact source name bound to the unsaved buffer.
        source_name: PathBuf,
        /// Caller-provided identity scratch bytes.
        provided: usize,
        /// Exact required identity scratch bytes.
        required: usize,
    },
    /// libclang tokenization exceeded the caller-derived token capacity.
    #[error(
        "libclang rejected {source_name:?}: tokenization observed {observed} tokens above the {limit} token capacity"
    )]
    LibclangTokenCapacity {
        /// Exact source name bound to the unsaved buffer.
        source_name: PathBuf,
        /// Observed token count.
        observed: usize,
        /// Caller-derived token capacity.
        limit: usize,
    },
    /// The native authority rejected the source with its first main-file error diagnostic.
    #[error("libclang rejected {source_name:?}")]
    ParseRejected {
        /// Exact source name bound to the unsaved buffer.
        source_name: PathBuf,
        /// First retained main-file error diagnostic when one exists.
        diagnostic: Option<ClangDiagnostic>,
    },
    /// Cancellation was observed at the named analysis phase.
    #[error("Clang analysis was cancelled during {phase:?}")]
    Cancelled {
        /// Exact phase observing the cancellation.
        phase: ClangPhase,
    },
    /// The deadline was exceeded at the named analysis phase.
    #[error("Clang analysis exceeded its deadline during {phase:?}")]
    DeadlineExceeded {
        /// Exact phase observing the deadline.
        phase: ClangPhase,
    },
    /// A source coordinate exceeded the addressable source extent.
    #[error("source coordinate {coordinate} exceeds the addressable source extent")]
    SourceCoordinateTooLarge {
        /// Exact rejected coordinate.
        coordinate: usize,
    },
    /// A source column exceeded the retained coordinate width.
    #[error("source column {column} on line {line} exceeds the coordinate width")]
    SourceCoordinateColumnTooLarge {
        /// Exact rejected line.
        line: u32,
        /// Exact rejected column.
        column: u32,
    },
    /// The diagnostic location had no representable line or column coordinate.
    #[error("source coordinate line {line} column {column} is unavailable")]
    SourceCoordinateUnavailable {
        /// Exact rejected line.
        line: u32,
        /// Exact rejected column.
        column: u32,
    },
    /// The diagnostic location fell outside the exact source bytes.
    #[error(
        "source coordinate line {line} column {column} is outside the {source_bytes}-byte source"
    )]
    SourceCoordinateOutOfBounds {
        /// Exact rejected line.
        line: u32,
        /// Exact rejected column.
        column: u32,
        /// Exact source byte count.
        source_bytes: usize,
    },
    /// Declaration nesting exceeded the caller traversal capacity.
    #[error("declaration nesting reaches depth {depth} above the traversal capacity")]
    AstNestingTooDeep {
        /// Exact rejected depth.
        depth: usize,
    },
    /// The interner rejected one more identity than the caller capacity admits.
    #[error("identity binding capacity {limit} was exceeded at {observed} entries")]
    BindingCapacity {
        /// Exact admitted caller capacity.
        limit: usize,
        /// Exact observed entry count including the rejected one.
        observed: usize,
    },
    /// A checked fact counter exceeded its address space.
    #[error("fact count exceeded the counter address space")]
    FactCountOverflow,
    /// A fact journal record written by this analysis did not decode; an internal invariant broke.
    #[error("fact journal record {ordinal} did not decode")]
    JournalRecordInvalid {
        /// Exact ordinal of the rejected journal record.
        ordinal: usize,
    },
}

impl From<ClangError<'_>> for ClangFailure {
    fn from(error: ClangError<'_>) -> Self {
        match error {
            #[cfg(not(clang_native))]
            ClangError::LibclangUnavailable => Self::LibclangUnavailable,
            ClangError::ToolVersionMismatch {
                expected_bytes,
                observed,
            } => Self::ToolVersionMismatch {
                expected_bytes,
                observed,
            },
            ClangError::LibclangIndexCreate => Self::LibclangIndexCreate,
            ClangError::LibclangArgumentNul => Self::LibclangArgumentNul,
            ClangError::LibclangSourceNul => Self::LibclangSourceNul,
            ClangError::LibclangParse { source_name, code } => Self::LibclangParse {
                source_name: source_name.to_path_buf(),
                code,
            },
            ClangError::LibclangIdentityUnavailable => Self::LibclangIdentityUnavailable,
            ClangError::LibclangExternalIdentityUnavailable { source_name } => {
                Self::LibclangExternalIdentityUnavailable {
                    source_name: source_name.to_path_buf(),
                }
            }
            ClangError::LibclangCallbackPanicked => Self::LibclangCallbackPanicked,
            ClangError::LibclangTraversalParentUnavailable => {
                Self::LibclangTraversalParentUnavailable
            }
            ClangError::LibclangFactScratchTooSmall {
                source_name,
                provided,
                required,
            } => Self::LibclangFactScratchTooSmall {
                source_name: source_name.to_path_buf(),
                provided,
                required,
            },
            ClangError::LibclangIdentityScratchTooSmall {
                source_name,
                provided,
                required,
            } => Self::LibclangIdentityScratchTooSmall {
                source_name: source_name.to_path_buf(),
                provided,
                required,
            },
            ClangError::LibclangTokenCapacity {
                source_name,
                observed,
                limit,
            } => Self::LibclangTokenCapacity {
                source_name: source_name.to_path_buf(),
                observed,
                limit,
            },
            ClangError::ParseRejected {
                source_name,
                diagnostic,
            } => Self::ParseRejected {
                source_name: source_name.to_path_buf(),
                diagnostic,
            },
            ClangError::Cancelled { phase } => Self::Cancelled { phase },
            ClangError::DeadlineExceeded { phase } => Self::DeadlineExceeded { phase },
            ClangError::SourceCoordinateTooLarge { coordinate } => {
                Self::SourceCoordinateTooLarge { coordinate }
            }
            ClangError::SourceCoordinateColumnTooLarge { line, column } => {
                Self::SourceCoordinateColumnTooLarge { line, column }
            }
            ClangError::SourceCoordinateUnavailable { line, column } => {
                Self::SourceCoordinateUnavailable { line, column }
            }
            ClangError::SourceCoordinateOutOfBounds {
                line,
                column,
                source_bytes,
            } => Self::SourceCoordinateOutOfBounds {
                line,
                column,
                source_bytes,
            },
            ClangError::AstNestingTooDeep { depth } => Self::AstNestingTooDeep { depth },
            ClangError::BindingCapacity { limit, observed } => {
                Self::BindingCapacity { limit, observed }
            }
            ClangError::FactCountOverflow => Self::FactCountOverflow,
            ClangError::JournalRecordInvalid { ordinal } => Self::JournalRecordInvalid { ordinal },
        }
    }
}
