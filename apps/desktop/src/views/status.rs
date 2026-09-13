//! The status bar: readiness, coverage, capabilities, owner mode, and jobs.
//! Every glyph in it is load-bearing; nothing here is decorative.
//! If the live feed is failing, it says so here and keeps saying so.
//!
//! A status bar earns its row of pixels only if a reader can answer real
//! questions from it without clicking: is the shelf current, which retrieval
//! lanes actually ran, which compiler oracles this build has, whether this
//! window owns the service or attached to one, and what is happening right
//! now. Everything here answers one of those.

use super::workspace::Workspace;
use crate::host::lease::HostMode;
use crate::store::jobs::{Job, JobState};
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Chrome, Space, TypeScale, hairline, space, type_size};
use crate::ui::{chip, fault as fault_ui, text};
use gpui::prelude::FluentBuilder as _;
use gpui::AppContext as _;
use gpui::{
    Context, Div, Stateful, IntoElement, InteractiveElement, ParentElement, StatefulInteractiveElement,
    Styled, div, px,
};

impl Workspace {
    /// Returns the status bar row.
    pub(super) fn status_bar(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let workspace = self.workspace.read(cx);
        let rows = workspace.row_count();
        let revision = workspace.revision().to_owned();
        let mode = workspace.mode();
        let endpoint = workspace.endpoint().spelling();
        let hydrating = workspace.hydrating();
        let fault = workspace.fault().cloned();
        let lanes: Vec<_> = workspace.coverage().to_vec();
        let totals = workspace.capability_totals();
        div()
            .flex_none()
            .h(px(Chrome::STATUS))
            .w_full()
            .flex()
            .items_center()
            .gap(space(Space::Base))
            .px(space(Space::Base))
            .border_t(hairline())
            .border_color(theme.paint(Paint::Hairline))
            .child(shelf_state(theme, rows, hydrating))
            .children(
                lanes
                    .iter()
                    .map(|lane| chip::coverage_chip(theme, lane).into_any_element()),
            )
            .child(capability_summary(theme, totals))
            .child(div().flex_1().min_w(px(0.0)).child(self.job_line(theme, cx)))
            .when_some(fault, |bar, fault| {
                bar.child(
                    div()
                        .flex_none()
                        .max_w(px(320.0))
                        .child(fault_ui::inline(theme, &fault)),
                )
            })
            .when(self.shell.read(cx).reduced_motion(), |bar| {
                bar.child(text::faint(theme).child("motion off"))
            })
            .child(owner_mode(theme, mode, &endpoint))
            .child(
                text::faint(theme)
                    .flex_none()
                    .font_family(theme.specimen())
                    .child(revision),
            )
    }

    fn job_line(&self, theme: &Theme, cx: &Context<Self>) -> Div {
        let Some(job) = self.jobs.read(cx).foreground() else {
            return div();
        };
        let role = match job.state() {
            JobState::Failed(_) => Paint::Fault,
            JobState::Submitted | JobState::Accepted => Paint::Caution,
        };
        div()
            .flex()
            .items_center()
            .gap(space(Space::Tight))
            .min_w(px(0.0))
            .child(
                div()
                    .flex_none()
                    .text_size(type_size(TypeScale::Micro))
                    .text_color(theme.paint(role))
                    .child(job_glyph(job)),
            )
            .child(
                text::single_line(text::faint(theme))
                    .flex_1()
                    .min_w(px(0.0))
                    .child(job.line()),
            )
    }
}

fn job_glyph(job: &Job) -> &'static str {
    match job.state() {
        JobState::Submitted => "○",
        JobState::Accepted => "◐",
        JobState::Failed(_) => "✗",
    }
}

fn shelf_state(theme: &Theme, rows: u64, hydrating: bool) -> Div {
    let (glyph, role) = if hydrating {
        ("◐", Paint::Caution)
    } else {
        ("✓", Paint::Ok)
    };
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(space(Space::Tight))
        .child(
            div()
                .text_size(type_size(TypeScale::Micro))
                .text_color(theme.paint(role))
                .child(glyph),
        )
        .child(text::faint(theme).child(format!("{rows} rows")))
}

fn capability_summary(theme: &Theme, totals: (usize, usize, usize)) -> Stateful<Div> {
    let (ready, probing, unavailable) = totals;
    div()
        .id("capability-summary")
        .flex()
        .flex_none()
        .items_center()
        .gap(space(Space::Tight))
        .child(mark(theme, "✓", ready, Paint::Ok))
        .child(mark(theme, "◐", probing, Paint::Caution))
        .child(mark(theme, "✗", unavailable, Paint::Info))
        .tooltip_show_delay(std::time::Duration::from_millis(300))
        .tooltip(move |_window, cx| {
            let theme = crate::theme::theme(cx);
            cx.new(|_| CapabilityTip {
                theme,
                ready,
                probing,
                unavailable,
            })
            .into()
        })
}

fn mark(theme: &Theme, glyph: &str, count: usize, role: Paint) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(2.0))
        .text_size(type_size(TypeScale::Micro))
        .text_color(theme.paint(role))
        .child(glyph.to_owned())
        .child(count.to_string())
}

fn owner_mode(theme: &Theme, mode: HostMode, endpoint: &str) -> Stateful<Div> {
    let label = match mode {
        HostMode::Embedded => "embedded owner",
        HostMode::Attached => "attached",
    };
    let endpoint = endpoint.to_owned();
    div()
        .id("owner-mode")
        .flex_none()
        .text_size(type_size(TypeScale::Micro))
        .text_color(theme.paint(Paint::TextFaint))
        .child(label)
        .tooltip_show_delay(std::time::Duration::from_millis(300))
        .tooltip(move |_window, cx| {
            let theme = crate::theme::theme(cx);
            let endpoint = endpoint.clone();
            cx.new(|_| EndpointTip { theme, endpoint }).into()
        })
}

struct CapabilityTip {
    theme: Theme,
    ready: usize,
    probing: usize,
    unavailable: usize,
}

impl gpui::Render for CapabilityTip {
    fn render(&mut self, _window: &mut gpui::Window, _cx: &mut Context<Self>) -> impl IntoElement {
        crate::ui::surface::raised(&self.theme)
            .px(space(Space::Base))
            .py(space(Space::Snug))
            .flex()
            .flex_col()
            .child(text::label(&self.theme).child("Compiler oracles and embedding"))
            .child(text::dim(&self.theme).child(format!(
                "{} ready · {} probing · {} unavailable",
                self.ready, self.probing, self.unavailable
            )))
    }
}

struct EndpointTip {
    theme: Theme,
    endpoint: String,
}

impl gpui::Render for EndpointTip {
    fn render(&mut self, _window: &mut gpui::Window, _cx: &mut Context<Self>) -> impl IntoElement {
        crate::ui::surface::raised(&self.theme)
            .px(space(Space::Base))
            .py(space(Space::Snug))
            .child(
                text::dim(&self.theme)
                    .font_family(self.theme.specimen())
                    .child(self.endpoint.clone()),
            )
    }
}
