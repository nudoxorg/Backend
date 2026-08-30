//! Actual GPUI entity/view adapter, kept behind an explicit feature.

use crate::{ApplicationReply, ApplyError, BatchReceipt, ShellState};
use gpui::{Context, IntoElement, Render, Window, div, prelude::*};

/// Entity-backed GPUI view for the bounded application shell.
pub struct GpuiShellView {
    state: ShellState,
}

impl GpuiShellView {
    /// Creates a shell with stable first-frame checking states.
    #[must_use]
    pub fn new(state: ShellState) -> Self {
        Self { state }
    }

    /// Borrows the projected state for assertions or parent composition.
    #[must_use]
    pub const fn state(&self) -> &ShellState {
        &self.state
    }

    /// Applies one bounded core reply slice and requests at most one GPUI notify.
    ///
    /// # Errors
    ///
    /// Returns [`ApplyError::BatchTooLarge`] when the slice exceeds the fixed shell boundary, or
    /// [`ApplyError::NotificationEpochExhausted`] when the notification counter cannot advance.
    pub fn apply_replies(
        &mut self,
        replies: &[ApplicationReply],
        cx: &mut Context<Self>,
    ) -> Result<BatchReceipt, ApplyError> {
        let receipt = self.state.apply_batch(replies)?;
        if receipt.notifications() != 0 {
            cx.notify();
        }
        Ok(receipt)
    }
}

impl Default for GpuiShellView {
    fn default() -> Self {
        Self::new(ShellState::default())
    }
}

impl Render for GpuiShellView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let summaries = self.state.summaries();
        div()
            .id("wave-application-shell")
            .flex()
            .flex_col()
            .gap_2()
            .children(summaries.into_iter().map(|summary| {
                div()
                    .id(summary.surface.element_id())
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(summary.surface.label())
                    .child(summary.state.label())
            }))
    }
}
