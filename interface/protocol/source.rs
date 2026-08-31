//! Defines source behavior for `interface-protocol`, whose purpose is to decode and project the shared application vocabulary for external transports.
//! This module owns the source invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Source-ingress vocabulary shared by the CLI parser and its process edge.

use std::num::NonZeroUsize;

use interface_core::{ApplicationInput, GenerateTarget};

/// One CLI command after structural parsing but before optional source-file or standard-input I/O.
#[derive(Debug, Eq, PartialEq)]
pub enum CliCommand {
    /// A fully admitted application request requiring no CLI-owned I/O.
    Application(ApplicationInput),
    /// One compiler target whose source is read from an exact filesystem path by the process edge.
    GenerateFile {
        /// Typed compiler target shared with every other generate path.
        target: GenerateTarget,
        /// Opaque path token interpreted only by the CLI process edge.
        path: String,
    },
    /// One compiler target whose source is read from the process standard-input edge.
    GenerateStandardInput {
        /// Typed compiler target shared with every other generate path.
        target: GenerateTarget,
    },
}

/// The process edge that owned a source acquisition attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceIngressRole {
    /// A path supplied to the CLI's `generate-file` command.
    FilePath,
    /// The CLI process standard input.
    StandardInput,
}

/// The exact I/O phase that produced a source-ingress failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceIngressPhase {
    /// Opening a named source file.
    Open,
    /// Reading stable length metadata from an already-open source file.
    Metadata,
    /// Reading an already-open source stream.
    Read,
}

/// Stable facts from a native source-ingress I/O failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceIoFact {
    /// Process edge that performed the failed operation.
    pub role: SourceIngressRole,
    /// I/O operation that failed.
    pub phase: SourceIngressPhase,
    /// Portable operating-system error classification.
    pub kind: std::io::ErrorKind,
    /// Platform error number when the operating system exposed one.
    pub raw_os_code: Option<i32>,
}

/// Exact invalid UTF-8 facts, retaining the bounded original bytes for recovery or reporting.
#[derive(Debug)]
pub struct SourceEncodingError {
    /// Original bounded source bytes, including the malformed sequence.
    pub bytes: Box<[u8]>,
    /// Last verified UTF-8 byte offset.
    pub valid_up_to: usize,
    /// Invalid sequence width when it is known by the decoder.
    pub error_length: Option<NonZeroUsize>,
}
