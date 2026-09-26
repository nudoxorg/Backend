//! Defines binding-store behavior for the `backend-engine` publication, whose purpose is to publish verified compiler fragments as immutable generations.
//! This module owns the binding-store invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Deterministically addressed immutable generation-to-manifest bindings.

use std::{
    io,
    path::{Path, PathBuf},
};

use thiserror::Error;

use crate::publication::binding::{CompilationBindingError, CompilationBindingFacts};

mod store;

const DIRECTORY: &str = "bindings";
const EXTENSION: &str = ".binding";
const HEX: &[u8; 16] = b"0123456789abcdef";
const TEMP_ATTEMPTS: u8 = 16;

/// Exact physical phase while storing or reopening a generation-addressed binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindingIoPhase {
    /// Create the binding directory.
    CreateDirectory,
    /// Open an existing deterministic binding path.
    OpenExisting,
    /// Read an existing binding.
    ReadExisting,
    /// Create a same-directory temporary binding.
    CreateTemporary,
    /// Write binding bytes to a temporary file.
    WriteTemporary,
    /// Sync temporary binding bytes.
    SyncTemporary,
    /// Rename the temporary binding to its deterministic generation address.
    PublishTemporary,
    /// Open the binding directory for a durability barrier.
    OpenDirectory,
    /// Sync the binding directory after a namespace transition.
    SyncDirectory,
    /// Remove a failed temporary binding.
    RemoveTemporary,
}

/// Failure while storing or loading an immutable generation-addressed binding.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum BindingStoreError {
    /// One exact binding filesystem phase failed.
    #[error("compiler binding I/O failed during {phase:?}")]
    Io {
        /// Exact physical phase.
        phase: BindingIoPhase,
        /// Original filesystem cause.
        #[source]
        source: io::Error,
    },
    /// Binding write failure and cleanup failure both remain available.
    #[error(
        "compiler binding I/O failed during {phase:?}; cleanup failed during {cleanup_phase:?}"
    )]
    IoWithCleanup {
        /// Primary write phase.
        phase: BindingIoPhase,
        /// Primary filesystem cause.
        #[source]
        source: io::Error,
        /// Cleanup phase.
        cleanup_phase: BindingIoPhase,
        /// Cleanup filesystem cause.
        cleanup_source: io::Error,
    },
    /// Existing deterministic binding did not have the complete fixed length.
    #[error("generation binding has {observed} bytes, expected {expected}")]
    Length {
        /// Fixed binding byte length required by this format.
        expected: usize,
        /// Observed file byte length.
        observed: u64,
    },
    /// Existing binding could not pass the immutable binding grammar.
    #[error("stored generation binding failed validation")]
    Binding(#[source] CompilationBindingError),
    /// Existing binding named a different verified generation than its deterministic path.
    #[error("stored generation binding facts disagree with its deterministic generation path")]
    GenerationMismatch {
        /// Generation facts derived from the deterministic path.
        expected: backend_store::hydration::VerifiedGenerationFacts,
        /// Generation facts carried by immutable binding bytes.
        observed: backend_store::hydration::VerifiedGenerationFacts,
    },
    /// Existing binding bytes had a different complete immutable identity or manifest fact.
    #[error("stored generation binding facts conflict with the requested immutable binding")]
    BindingMismatch {
        /// Facts requested for the immutable generation address.
        expected: CompilationBindingFacts,
        /// Existing validated facts at that address.
        observed: CompilationBindingFacts,
    },
    /// Every bounded temporary binding name collided with a pre-existing temporary path.
    #[error("could not allocate a temporary compiler binding after {attempts} attempts")]
    TemporaryNamesExhausted {
        /// Fixed bounded collision attempts.
        attempts: u8,
        /// Last exact create-new collision cause.
        #[source]
        source: io::Error,
    },
}

/// Location and validated facts of a generation-addressed immutable binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredBinding {
    /// Validated complete immutable binding facts.
    pub facts: CompilationBindingFacts,
    /// Deterministic path derived only from the bound generation root and dependency set.
    pub path: PathBuf,
}

/// Single-owner store for deterministic `{pinned_root, dep_set}` binding paths.
#[derive(Debug)]
pub(crate) struct GenerationBindingStore {
    directory: PathBuf,
    next_temporary: u64,
}

fn sync_directory(directory: &Path) -> Result<(), (BindingIoPhase, io::Error)> {
    let file = backend_platform::durability::open_directory(directory)
        .map_err(|source| (BindingIoPhase::OpenDirectory, source))?;
    file.sync_all()
        .map_err(|source| (BindingIoPhase::SyncDirectory, source))
}

fn hexadecimal(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[usize::from(*byte >> 4)] as char);
        output.push(HEX[usize::from(*byte & 0x0f)] as char);
    }
    output
}

fn initial_nonce() -> u64 {
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX)
        });
    time ^ u64::from(std::process::id()).rotate_left(17)
}
