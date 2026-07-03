//! Our module for handling and communicating progress.
//! This is designed to give both users and our logs a full picture into where
//! things stand during package parse.
//!
//! A little over-engineered, sure, but provides a real outlook into how we
//! should position things.

use serde::{Deserialize, Serialize};

use crate::lifecycle::{Phase, ResolutionState};

#[nutype::nutype(
	validate(less_or_equal = 100),
	derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)
)]
// TODO: Refinement types?
pub struct Percent(u8);

impl Percent {
	/// Full progress.
	pub const FULL: u8 = 100;
	/// Zero progress.
	pub const ZERO: u8 = 0;

	/// Whether this represents completion.
	pub fn is_full(self) -> bool { self.into_inner() == Self::FULL }
}

pub enum ProgressStatus<P> {
	Unstarted,
	Progressing { phase: P, fraction: Percent },
	Completed,
	Failed { phase: P },
}

/// Anything that can report progress through a phased operation. The observer
/// hooks let callers react to completion without polling.
pub trait Progressive {
	/// The phase type this operation moves through.
	type Phase: Clone + PartialEq + 'static;

	/// The ordered sequence of phases this operation goes through.
	const PHASES: &'static [Self::Phase];

	/// Translates the implementer's internal state into a standard progress
	/// status.
	fn status(&self) -> ProgressStatus<Self::Phase>;

	/// The current phase, or `None` if not started / finished / failed.
	fn current_phase(&self) -> Option<Self::Phase> {
		match self.status() {
			ProgressStatus::Progressing { phase, .. } => Some(phase),
			_ => None,
		}
	}

	/// Progress within the current phase.
	fn phase_progress(&self) -> Percent {
		match self.status() {
			ProgressStatus::Progressing { fraction, .. } => fraction,
			ProgressStatus::Completed => Percent::try_new(Percent::FULL).expect("100 valid"),
			_ => Percent::try_new(Percent::ZERO).expect("0 valid"),
		}
	}

	/// Overall progress across all phases.
	fn overall(&self) -> Percent {
		let phases = Self::PHASES;
		let n = phases.len() as u32;

		if n == 0 {
			return Percent::try_new(Percent::FULL).expect("100 valid");
		}

		let pct = match self.status() {
			ProgressStatus::Unstarted => 0,
			ProgressStatus::Completed => 100,
			ProgressStatus::Progressing { phase, fraction } => {
				let done = phases.iter().position(|p| *p == phase).unwrap_or(0) as u32;
				let frac = fraction.into_inner() as u32;
				(done * 100 + frac) / n
			}
			ProgressStatus::Failed { phase } => {
				let done = phases.iter().position(|p| *p == phase).unwrap_or(0) as u32;
				(done * 100) / n
			}
		};

		Percent::try_new(pct.min(100) as u8).expect("clamped to 0..=100")
	}

	/// Whether the whole operation has completed.
	fn is_complete(&self) -> bool { self.overall().is_full() }

	/// Invoke `on_done` IF the operation has completed.
	fn when_complete(&self, on_done: impl FnOnce()) {
		if self.is_complete() {
			on_done();
		}
	}
}

/// The progress of a package indexing job!
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobProgress {
	/// The authoritative persisted state.
	pub state: ResolutionState,

	/// How far through [`ResolutionState::Progressing`]'s phase we are. Ignored
	/// for terminal states.
	pub phase_fraction: Percent,
}

impl Progressive for JobProgress {
	type Phase = Phase;

	const PHASES: &'static [Phase] =
		&[Phase::Acquiring, Phase::Extracting, Phase::Compiling, Phase::Emitting];

	fn status(&self) -> ProgressStatus<Phase> {
		match &self.state {
			ResolutionState::Unindexed { .. } => ProgressStatus::Unstarted,
			ResolutionState::Progressing(phase) => {
				ProgressStatus::Progressing { phase: *phase, fraction: self.phase_fraction }
			}
			ResolutionState::Stored { .. } => ProgressStatus::Completed,
			ResolutionState::Failed(f) | ResolutionState::DeadLettered(f) => {
				ProgressStatus::Failed { phase: f.phase }
			}
		}
	}
}
