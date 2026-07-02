//! Progress reporting — a *view* derived from the persisted lifecycle, not a
//! second source of truth.
//!
//! The authoritative state is [`crate::lifecycle::ResolutionState`]. Anything
//! long-running (an indexing job, a store poller, a blob build) reports live
//! progress by implementing [`Progressive`], whose overall percentage is
//! computed from the phase ordinal plus intra-phase fraction — so progress can
//! never disagree with the state machine.

use serde::{Deserialize, Serialize};

use crate::lifecycle::{Phase, ResolutionState};

/// A percentage in `0..=100`. Out-of-range values are unrepresentable, so
/// progress arithmetic can never produce a nonsensical `147%`.
#[nutype::nutype(
	validate(less_or_equal = 100),
	derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)
)]
pub struct Percent(u8);

impl Percent {
	/// Zero progress.
	pub const ZERO: u8 = 0;
	/// Full progress.
	pub const FULL: u8 = 100;

	/// Whether this represents completion.
	pub fn is_full(self) -> bool { self.into_inner() == Self::FULL }
}

/// Anything that can report progress through a phased operation. The observer
/// hooks let callers react to completion without polling.
pub trait Progressive {
	/// The phase type this operation moves through.
	type Phase: Clone + PartialEq;

	/// The current phase, or `None` if not started / finished / failed.
	fn current_phase(&self) -> Option<Self::Phase>;

	/// Overall progress across all phases.
	fn overall(&self) -> Percent;

	/// Progress within the current phase.
	fn phase_progress(&self) -> Percent;

	/// Whether the whole operation has completed.
	fn is_complete(&self) -> bool { self.overall().is_full() }

	/// Invoke `on_done` iff the operation has completed.
	fn when_complete(&self, on_done: impl FnOnce()) {
		if self.is_complete() {
			on_done();
		}
	}
}

/// The live progress of a package indexing job — the concrete [`Progressive`]
/// the pipeline reports through. Wraps the persisted state plus how far the
/// current phase has advanced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobProgress {
	/// The authoritative persisted state.
	pub state: ResolutionState,
	/// How far through [`ResolutionState::Progressing`]'s phase we are. Ignored
	/// for terminal states.
	pub phase_fraction: Percent,
}

impl JobProgress {
	/// The ordered pipeline phases, used to weight overall progress.
	pub const PHASES: [Phase; 4] =
		[Phase::Acquiring, Phase::Extracting, Phase::Compiling, Phase::Emitting];

	/// The zero-based index of a phase in the pipeline.
	fn phase_index(phase: Phase) -> usize {
		Self::PHASES.iter().position(|p| *p == phase).unwrap_or(0)
	}
}

impl Progressive for JobProgress {
	type Phase = Phase;

	fn current_phase(&self) -> Option<Phase> {
		match &self.state {
			ResolutionState::Progressing(phase) => Some(*phase),
			_ => None,
		}
	}

	fn overall(&self) -> Percent {
		let n = Self::PHASES.len() as u32;
		let pct = match &self.state {
			ResolutionState::Unindexed { .. } => 0,
			ResolutionState::Progressing(phase) => {
				let done = Self::phase_index(*phase) as u32;
				let frac = self.phase_fraction.into_inner() as u32; // 0..=100 within the phase
				(done * 100 + frac) / n
			}
			ResolutionState::Stored { .. } => 100,
			// A failure freezes progress at the phases completed before it failed.
			ResolutionState::Failed(f) | ResolutionState::DeadLettered(f) => {
				(Self::phase_index(f.phase) as u32 * 100) / n
			}
		};
		Percent::try_new(pct.min(100) as u8).expect("clamped to 0..=100")
	}

	fn phase_progress(&self) -> Percent {
		match &self.state {
			ResolutionState::Progressing(_) => self.phase_fraction,
			ResolutionState::Stored { .. } => Percent::try_new(Percent::FULL).expect("100 valid"),
			_ => Percent::try_new(Percent::ZERO).expect("0 valid"),
		}
	}
}
