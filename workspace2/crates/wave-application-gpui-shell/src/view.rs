//! Actual GPUI entity/view adapter, kept behind an explicit feature.

use crate::{ApplicationInput, ApplicationReply, ApplicationService, ApplyError, ShellState};
use gpui::{Context, IntoElement, Render, Window, div, prelude::*};

/// Entity-backed GPUI view for the bounded application shell.
///
/// The entity owns the one concrete application service used by the UI. A caller supplies typed
/// [`ApplicationInput`] values; this view never decodes a second command vocabulary or searches
/// for a backend. The service reply is projected into fixed state and emits one bounded notify.
pub struct GpuiShellView {
    service: ApplicationService,
    state: ShellState,
}

impl GpuiShellView {
    /// Creates a shell around one application service owner.
    #[must_use]
    pub fn new(service: ApplicationService) -> Self {
        Self {
            service,
            state: ShellState::default(),
        }
    }

    /// Borrows the projected state for assertions or parent composition.
    #[must_use]
    pub const fn state(&self) -> &ShellState {
        &self.state
    }

    /// Executes one typed application command and projects its exact reply.
    ///
    /// The service is entity-owned, so sequential GPUI inputs share operation ownership and
    /// cancellation state. No task, timer, global lookup, or accessibility driver is introduced
    /// by the shell.
    ///
    /// # Errors
    ///
    /// Returns [`ApplyError::NotificationEpochExhausted`] only if the fixed notification epoch
    /// cannot advance.
    pub fn execute(
        &mut self,
        input: &ApplicationInput,
        cx: &mut Context<Self>,
    ) -> Result<ApplicationReply, ApplyError> {
        let reply = self.service.execute(input);
        self.state.apply_batch(&[reply])?;
        cx.notify();
        Ok(reply)
    }
}

impl Default for GpuiShellView {
    fn default() -> Self {
        Self::new(ApplicationService::new())
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
