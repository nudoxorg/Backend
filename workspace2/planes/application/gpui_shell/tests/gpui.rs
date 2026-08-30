//! Deterministic actual-GPUI entity tests.

#[cfg(feature = "real-gpui")]
use gpui::{AppContext, TestAppContext};
#[cfg(feature = "real-gpui")]
use wave_application_core::{
    ApplicationReply, Capability, CapabilityHealth, CorrelationId, ProgressPage, ReplyBody,
    Terminal,
};
#[cfg(feature = "real-gpui")]
use wave_application_gpui_shell::{GpuiShellView, ProjectionState, ShellState};

#[cfg(feature = "real-gpui")]
#[gpui::test]
fn entity_view_projects_core_health_and_requests_one_notify(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_, _| GpuiShellView::new(ShellState::default()));
    let incoming = ApplicationReply {
        correlation: CorrelationId(91),
        body: ReplyBody::Health([
            CapabilityHealth::LocalReady(Capability::Compiler),
            CapabilityHealth::Unavailable(Capability::Index),
            CapabilityHealth::Unavailable(Capability::Graph),
            CapabilityHealth::Unavailable(Capability::Vector),
        ]),
        terminal: Terminal::Complete { emitted: 4 },
        diagnostic: None,
    };

    let result = view.update(cx, |view, cx| view.apply_replies(&[incoming], cx));
    assert_eq!(
        result.map(|receipt| (receipt.applied_replies(), receipt.notifications())),
        Ok((1, 1))
    );
    cx.read_entity(&view, |view, _| {
        assert_eq!(view.state().last_correlation(), Some(CorrelationId(91)));
        assert_eq!(
            view.state().summaries()[4].state,
            ProjectionState::Degraded(Capability::Index)
        );
    });
}

#[cfg(feature = "real-gpui")]
#[gpui::test]
fn entity_view_projects_progress_page_with_stable_first_frame(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_, _| GpuiShellView::default());
    let incoming = ApplicationReply {
        correlation: CorrelationId(92),
        body: ReplyBody::Progress(ProgressPage::Finished),
        terminal: Terminal::Complete { emitted: 1 },
        diagnostic: None,
    };

    cx.read_entity(&view, |view, _| {
        assert!(
            view.state()
                .summaries()
                .into_iter()
                .all(|summary| summary.state == ProjectionState::Checking)
        );
    });
    let result = view.update(cx, |view, cx| view.apply_replies(&[incoming], cx));
    assert_eq!(
        result.map(|receipt| (receipt.applied_replies(), receipt.notifications())),
        Ok((1, 1))
    );
    cx.read_entity(&view, |view, _| {
        assert_eq!(view.state().summaries()[5].state, ProjectionState::Ready);
    });
}
