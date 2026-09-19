//! The status bar: present only while something is not done.
//! A job in flight, a project indexing, a feed that is failing, a snapshot
//! being hydrated — each earns the row. When nothing is happening the row is
//! not drawn at all, because "ready" is the state of the world and a bar
//! that announces the state of the world is furniture.
//!
//! Diagnostics — capabilities, lanes, endpoint, revision, owner mode — live in
//! Settings › Diagnostics. They are true, they are worth having, and they are
//! not worth a permanent strip of pixels under every page.

use super::workspace::Workspace;
use crate::presentation::project::{self, Standing};
use crate::store::jobs::{Job, JobState};
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Chrome, Space, TypeScale, hairline, space, type_size};
use crate::ui::{fault as fault_ui, text};
use backend_present::{Readiness, ShelfEntry};
use gpui::prelude::FluentBuilder as _;
use gpui::{AnyElement, Context, Div, IntoElement, ParentElement, Styled, div, px};

/// One thing the bar has to say.
enum Line {
    Job(Box<Job>),
    Indexing(Box<ShelfEntry>),
    Hydrating,
}

impl Workspace {
    /// Returns the status bar, or nothing when there is nothing to say.
    pub(super) fn status_bar(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let lines = self.status_lines(cx);
        let fault = self.engine.read(cx).fault().cloned();
        if lines.is_empty() && fault.is_none() {
            return div().into_any_element();
        }
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
            .children(lines.iter().take(3).map(|line| status_line(theme, line)))
            .child(div().flex_1())
            .when_some(fault, |bar, fault| {
                bar.child(
                    div()
                        .flex_none()
                        .max_w(px(420.0))
                        .child(fault_ui::inline(theme, &fault)),
                )
            })
            .into_any_element()
    }

    fn status_lines(&self, cx: &Context<Self>) -> Vec<Line> {
        let mut lines = Vec::new();
        if self.engine.read(cx).hydrating() {
            lines.push(Line::Hydrating);
        }
        if let Some(job) = self.jobs.read(cx).foreground() {
            lines.push(Line::Job(Box::new(job.clone())));
        }
        let merged = self.jobs.read(cx).merge(self.engine.read(cx).shelf());
        for entry in merged.entries() {
            if matches!(project::standing(entry), Standing::Indexing | Standing::Requested)
                && !lines.iter().any(|line| matches!(line, Line::Job(_)))
            {
                lines.push(Line::Indexing(Box::new(entry.clone())));
            }
        }
        lines
    }
}

fn status_line(theme: &Theme, line: &Line) -> Div {
    let (glyph, role, words) = match line {
        Line::Hydrating => ("◐", Paint::Caution, "Reading the shelf…".to_owned()),
        Line::Job(job) => (
            job_glyph(job),
            match job.state() {
                JobState::Failed(_) => Paint::Fault,
                JobState::Submitted | JobState::Accepted => Paint::Caution,
            },
            job.line(),
        ),
        Line::Indexing(entry) => (
            "◐",
            Paint::Caution,
            match entry.readiness() {
                Readiness::Indexing { rows } => format!(
                    "Indexing {} · {} declarations so far",
                    entry.identity().name(),
                    rows.get()
                ),
                _ => format!("Waiting for {}…", entry.identity().name()),
            },
        ),
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
                .child(glyph),
        )
        .child(
            text::single_line(text::faint(theme))
                .max_w(px(360.0))
                .child(words),
        )
}

fn job_glyph(job: &Job) -> &'static str {
    match job.state() {
        JobState::Submitted => "○",
        JobState::Accepted => "◐",
        JobState::Failed(_) => "✗",
    }
}
