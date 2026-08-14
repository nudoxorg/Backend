//! Indexing phases, recorded failures, and resolution states.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::content::ContentHash;

/// The distinct phases of indexing a package, in order.
///
/// The wire token for each variant is its lowercase name (`"acquiring"`, etc.),
/// matching the postgres `CHECK` domain.  `VariantNames::VARIANTS` is the
/// single source the schema CHECK constraint is derived from.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    strum::Display,
    strum::EnumIter,
    strum::EnumString,
    strum::IntoStaticStr,
    strum::VariantNames,
)]
#[strum(serialize_all = "lowercase")]
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

/// Structured cause captured for a failure without storing an untyped
/// `Box<dyn Error>` or bare ad-hoc String. The human message is always
/// derived from the cause at construction time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "info")]
pub enum ErrorDetails {
    /// Fallback for cases where only a rendered message was captured.
    /// Prefer more specific variants when adding new failure paths.
    Message(String),
}

/// A recorded failure, with enough context to decide retry vs dead-letter.
/// The serialized shape preserves the legacy "error" key for DB roundtrips.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    /// How many attempts have been made so far.
    pub attempts: u32,
    /// The phase the most recent attempt failed in.
    pub phase: Phase,
    /// Human-readable message (rendered from the cause). Serialized under
    /// the key "error" for persistence compatibility.
    #[serde(rename = "error")]
    pub message: String,
    /// Structured details about the cause (enables future richer capture
    /// without losing source info in the Rust error chain at call sites).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<ErrorDetails>,
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
