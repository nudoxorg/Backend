//! The single, canonical lifecycle vocabulary for a package moving through the
//! pipeline. Replaces the overlapping `Progressive` / `ResolutionState` /
//! `Phase` triad with one persisted state machine; any display-progress is
//! *derived* from this, never tracked separately.
//!
//! Crucially, unlike the original, this models **failure**: a package whose
//! parse crashes, hangs, or exhausts retries has a legal, terminal state, so
//! the queue can neither livelock on a poison pill nor silently drop it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::content::ContentHash;

/// Where a package sits in the pipeline. Persisted in postgres as the source of
/// truth for orchestration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolutionState {
	/// No indexing attempted yet.
	Unindexed {
		/// A related package's indexing needs this one, but it is still untouched
		/// (drives dependency-ordered scheduling).
		needed: bool,
	},

	/// Indexing is in progress, currently in the given phase.
	Progressing(Phase),

	/// All phases complete; stored and actionable, at the given content hash.
	Stored { hash: ContentHash },

	/// Indexing failed. Retriable until `attempts` hits the policy ceiling.
	Failed(Failure),

	/// Terminally failed and quarantined — removed from the live queue, awaiting
	/// operator inspection. The escape hatch that makes poison pills bounded.
	DeadLettered(Failure),
}

/// The distinct phases of indexing a package, in order. Progress within a phase
/// is a derived `0..=100`, not stored per-phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::Display, strum::EnumIter)]
pub enum Phase {
	/// Resolving the concrete version + downloading the source archive.
	Acquiring,
	/// Extracting + sanitizing the (untrusted) source archive.
	Extracting,
	/// The compiler/oracle is lowering source to IR.
	Compiling,
	/// Fanning the parsed result out to the derived stores.
	Emitting,
}

/// A recorded failure, with enough context to decide retry vs dead-letter and
/// to debug after the fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
	/// How many attempts have been made so far.
	pub attempts: u32,
	/// The phase the most recent attempt failed in.
	pub phase: Phase,
	/// The classified kind of failure (drives retry policy).
	pub kind: FailureKind,
	/// A human-readable rendering of the underlying error.
	pub message: String,
	/// When the most recent attempt failed.
	pub at: DateTime<Utc>,
}

/// The class of a failure — what the retry policy branches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::Display)]
pub enum FailureKind {
	/// Transient (network, timeout, backend 5xx) — retry with backoff.
	Transient,
	/// The source could not be acquired (404, yanked, gone) — terminal.
	SourceUnavailable,
	/// The producer/oracle rejected the source (parse error) — terminal.
	Malformed,
	/// The job exceeded its wall-clock budget — retry a bounded number of times.
	Timeout,
	/// The archive tripped a safety limit (bomb, traversal) — terminal + flagged.
	Unsafe,
	/// An internal invariant broke — terminal, alert-worthy.
	Internal,
}

impl FailureKind {
	/// Whether a failure of this kind should be retried at all (before the
	/// attempt ceiling is even consulted).
	pub const fn is_retriable(self) -> bool {
		matches!(self, FailureKind::Transient | FailureKind::Timeout)
	}
}
