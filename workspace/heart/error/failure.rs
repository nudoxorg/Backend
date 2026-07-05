use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::content::ContentHash;

/// The distinct phases of indexing a package, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, strum::Display, strum::EnumIter)]
pub enum Phase {
    /// Resolving the concrete version + downloading the source archive.
    Acquiring,
    /// Extracting + sanitizing the (untrusted) source archive.
    Extracting,
    /// The compiler is lowering source to IR.
    Compiling,
    /// Fanning the parsed result out to derived stores.
    Emitting,
}

/// A recorded failure, with enough context to decide retry vs dead-letter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    /// How many attempts have been made so far.
    pub attempts: u32,
    /// The phase the most recent attempt failed in.
    pub phase: Phase,
    /// Human-readable error message from the failed attempt.
    pub error: String,
    /// When the most recent attempt failed.
    pub at: DateTime<Utc>,
}

/// Failure classes
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, strum::Display)]
pub enum FailureKind {
    Transient,
    SourceUnavailable,
    Malformed,
    Timeout,
    Unsafe,
    Internal,
}

impl FailureKind {
    pub const fn is_retriable(self) -> bool {
        matches!(self, FailureKind::Transient | FailureKind::Timeout)
    }
}

/// Where a package sits in the indexing pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolutionState {
    /// No indexing attempted yet.
    Unindexed { needed: bool },
    /// Indexing is in progress, currently in the given phase.
    Progressing(Phase),
    /// All phases complete; stored and actionable, at the given content hash.
    Stored { hash: ContentHash },
    /// Indexing failed. Retriable until attempts hits the policy ceiling.
    Failed(Failure),
    /// Failed and waiting for a human to inspect.
    DeadLettered(Failure),
}
