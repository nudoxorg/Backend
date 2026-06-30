//! The generic progress-reporting trait, shared by any subsystem that holds an
//! in-between state.

/// Our trait for anything that can communicate progress or hold an in-between
/// state
pub trait Progressive {
	/// The type representing distinct phases of the operation.
	/// For non-phasic operations, this can simply be defined as `()`.
	type Phase: Clone + PartialEq;

	/// The information the item is observing to update its internal progress, can
	/// be just the item itself. This should now encompass data that might
	/// trigger a phase transition.
	type Change;

	/// Get the current phase the operation is in.
	/// Returns `None` if the operation hasn't started, is in an unknown state, or
	/// is fully complete (depending on the specific implementation's design).
	fn current_phase(&self) -> Option<Self::Phase>;

	/// Get the progress of this struct based on an internal calculation (Overall
	/// progress: 0-100)
	fn get_progress(&self) -> u8;

	/// Get the progress of the *current phase* based on an internal calculation
	/// (0-100). If the item is not in an active phase, this should typically
	/// return 0 or 100.
	fn get_phase_progress(&self) -> u8;

	/// Update the progress of the object with a new variant of the struct,
	/// diffing and incorporating. This method is responsible for handling both
	/// intra-phase progress and inter-phase transitions.
	fn update_progress(&mut self, information: Self::Change);

	/// Returns `true` if progress has reached a terminal state.
	fn is_complete(&self) -> bool { self.get_progress() >= 100 }

	/// Returns `true` if the current phase has reached a terminal state.
	fn is_phase_complete(&self) -> bool { self.get_phase_progress() >= 100 }

	/// The action to take when the progress has reached a natural end or a
	/// stopping point. Called when progress reaches a natural end.
	fn on_complete(&self, mut callback: impl FnMut()) {
		if self.is_complete() {
			callback();
		}
	}

	/// The action to take when a specific phase reaches its end.
	/// Useful for triggering phase-specific side-effects (e.g., logging "Download
	/// complete, extracting...").
	fn on_phase_complete(&self, mut callback: impl FnMut(Self::Phase)) {
		if self.is_phase_complete() {
			if let Some(phase) = self.current_phase() {
				callback(phase);
			}
		}
	}
}
