//! The generic progress-reporting trait, shared by any subsystem that holds an
//! in-between state.

/// Our trait for anything that can communicate progress or hold an in-between state
pub trait Progressive {
	// TODO: Support phasic behavior optionally
	/// The information the item is observing to update its internal progress, can be just the item itself.
	type Change;

	/// Get the progress of this struct based on an internal calculation
	fn get_progress(&self) -> u8;

	/// Update the progress of the object with a new variant of the struct, diffing and incorporating
	fn update_progress(&mut self, information: Self::Change);

	/// Returns `true` if progress has reached a terminal state.
    fn is_complete(&self) -> bool {
        self.get_progress() >= 100
    }

	/// The action to take when the progress has reached a natural end or a stopping point.
	/// Called when progress reaches a natural end.
    fn on_complete(&self, callback: impl FnMut());
}
