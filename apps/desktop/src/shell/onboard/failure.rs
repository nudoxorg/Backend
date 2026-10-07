//! A project needing attention: a stopped attempt or an exact publication
//! with unavailable profiles, with typed detail one press away.
//!
//! Failure wording comes from the same typed project lifecycle as the shelf,
//! header and onboarding. Raw owner detail is retained behind disclosure;
//! it cannot establish a compiler cause or retained publication by its words.

use super::commands::{ProjectCommand, Weight};
use crate::core::LocalProjectId;
use crate::model::{ProjectLifecycle, WorkspaceProject};
use crate::shell::bodies::{Ctx, Leaf};
use crate::shell::focus::{Act, Target, native_control};
use crate::shell::kit::text;
use crate::shell::reader::Reader;
use facet::Space;
use facet::controls::button;
use facet::icons::{Icon, IconSize, ui};
use facet::tokens::ty;
use gpui::{
    BorrowAppContext as _, Context, Global, InteractiveElement as _, IntoElement as _,
    ParentElement, SharedString, StatefulInteractiveElement as _, Styled, div, px,
};
use std::collections::HashSet;
use std::rc::Rc;

/// Which failed projects show the owner's words.
#[derive(Default)]
struct Disclosed(HashSet<LocalProjectId>);

impl Global for Disclosed {}

/// Attention cards include partial publications while keeping usable navigation.
pub(crate) fn stopped(
    projects: &[WorkspaceProject],
    ctx: &mut Ctx<'_>,
    cx: &mut Context<Reader>,
) -> Option<Leaf> {
    let cards: Vec<_> = projects
        .iter()
        .filter(|project| {
            matches!(
                project.lifecycle(),
                ProjectLifecycle::Failed(_)
                    | ProjectLifecycle::PartiallyPublished
                    | ProjectLifecycle::CompilerRefused(_)
                    | ProjectLifecycle::Paused
                    | ProjectLifecycle::Missing
                    | ProjectLifecycle::CheckingOutcome
                    | ProjectLifecycle::UnknownOutcome
                    | ProjectLifecycle::OutsideReceiptWindow
                    | ProjectLifecycle::Unresolved
            )
        })
        .collect();
    if cards.is_empty() {
        return None;
    }
    let mut block = div()
        .flex()
        .flex_col()
        .items_center()
        .gap(ctx.measure.space(Space::Wide))
        .py(ctx.measure.space(Space::Wide));
    for project in cards {
        block = block.child(card(project, ctx, cx));
    }
    Some(Leaf::new(block))
}

fn card(
    project: &WorkspaceProject,
    ctx: &mut Ctx<'_>,
    cx: &mut Context<Reader>,
) -> gpui::AnyElement {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let lifecycle = project.lifecycle();
    let owner_words = project.partial_refusal_words().or_else(|| project
        .error
        .as_deref()
        .filter(|words| !words.is_empty())
        .map(str::to_owned));
    let headline = format!("{}: {}.", project.label, lifecycle.label());
    let mut lines = vec![lifecycle.detail().to_owned()];
    if lifecycle == ProjectLifecycle::Missing {
        lines.push(format!("Nudox looked for it at {}.", project.path));
    }
    let headline = ctx.say(headline);
    let native_status = headline.clone();
    let mut stack = div()
        .flex()
        .flex_col()
        .items_center()
        .gap(measure.space(Space::Snug))
        .max_w(px(560.0 * measure.scale()));
    stack = stack.child(
        div()
            .flex()
            .items_center()
            .gap(measure.space(Space::Base))
            .child(ui(Icon::Alert, IconSize::S14, palette.coral.base).size(measure.icon(14.0)))
            .child(
                text(ty::ROW, &measure, palette.ink0)
                    .role(gpui::Role::Status)
                    .aria_label(native_status)
                    .child(headline),
            ),
    );
    for line in lines {
        let line = ctx.say(line);
        stack = stack.child(
            text(ty::SMALL, &measure, palette.ink2)
                .text_center()
                .child(line),
        );
    }
    let id = project.id.clone();
    let mut actions = div()
        .flex()
        .flex_wrap()
        .justify_center()
        .gap(measure.space(Space::Base))
        .pt(measure.space(Space::Snug));
    for command in
        ProjectCommand::for_project(project, false, ctx.links.store.read(cx).owner_serving())
            .into_iter()
            .filter(|command| *command != ProjectCommand::Activate)
    {
        actions = actions.child(act(
            ctx,
            format!("{command:?}-{}", project.path).to_lowercase(),
            command,
            &id,
            cx,
        ));
    }
    stack = stack.child(actions);
    if let Some(words) = owner_words {
        let shown = cx.default_global::<Disclosed>().0.contains(&id);
        let reader = cx.entity();
        let toggle = id.clone();
        let label = ctx.say(match (lifecycle, shown) {
            (ProjectLifecycle::PartiallyPublished, true) => "Hide profiles needing attention",
            (ProjectLifecycle::PartiallyPublished, false) => "Profiles needing attention",
            (_, true) => "Hide the index's own words",
            (_, false) => "The index's own words",
        });
        let door = format!("owner-words-{}", project.path);
        let act: Act = Rc::new(move |_, cx| {
            cx.update_global(|disclosed: &mut Disclosed, _| {
                if !disclosed.0.remove(&toggle) {
                    disclosed.0.insert(toggle.clone());
                }
            });
            reader.update(cx, |_, cx| cx.notify());
        });
        let target_action = ctx.target_local_action(act, cx);
        let act = target_action.callback();
        ctx.targets.push(Target {
            id: door.clone().into(),
            label: label.clone(),
            action: target_action.clone(),
            peek: None,
            source: None,
        });
        let focus = ctx.native_handle(&SharedString::from(door.clone()), cx);
        let face = native_control(
            door.clone().into(),
            label.clone(),
            gpui::Role::Button,
            focus,
            Rc::clone(&act),
        )
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_center()
        .min_h(px(24.0 * measure.scale()))
        .px(measure.space(Space::Roomy))
        .hover(|style| style.bg(palette.tint))
        .child(text(ty::SMALL, &measure, palette.ink2).child(label));
        stack = stack.child(ctx.targets.track(door.clone(), face));
        if shown {
            let words = ctx.say(words);
            stack = stack.child(
                div()
                    .w_full()
                    .p(measure.space(Space::Roomy))
                    .bg(palette.plate)
                    .child(text(ty::MONO_SMALL, &measure, palette.ink1).child(words)),
            );
        }
    }
    stack.into_any_element()
}

/// One button that dispatches the command's intent for `project`, and the
/// keyboard's way to it.
fn act(
    ctx: &mut Ctx<'_>,
    id: String,
    command: ProjectCommand,
    project: &LocalProjectId,
    cx: &mut Context<Reader>,
) -> gpui::AnyElement {
    let links = ctx.links.clone();
    let intent = command.intent(project);
    let expected = ctx
        .links
        .snapshot(cx)
        .workspace()
        .projects
        .iter()
        .find(|row| row.id == *project)
        .and_then(|row| row.operation.clone());
    let project = project.clone();
    let act: Act = Rc::new(move |_, cx| {
        if matches!(
            command,
            ProjectCommand::CheckOutcome | ProjectCommand::StartNewIndex
        ) && !links.snapshot(cx).workspace().projects.iter().any(|row| {
            row.id == project
                && row.phase == crate::model::ProjectPhase::Unconfirmed
                && row.request.is_none()
                && row
                    .operation
                    .as_ref()
                    .zip(expected.as_ref())
                    .is_some_and(|(saved, expected)| {
                        saved.same_request(expected)
                            && saved.belongs_to(&project)
                            && (command != ProjectCommand::StartNewIndex
                                || (saved.permits_new_attempt() && expected.permits_new_attempt()))
                    })
        }) {
            return;
        }
        links.dispatch(intent.clone(), cx);
    });
    let target_action = if matches!(
        command,
        ProjectCommand::CheckOutcome | ProjectCommand::StartNewIndex
    ) {
        ctx.target_snapshot_action(act, cx)
    } else {
        ctx.target_local_action(act, cx)
    };
    let act = target_action.callback();
    ctx.targets.push(Target {
        id: id.clone().into(),
        label: command.label().into(),
        action: target_action,
        peek: None,
        source: None,
    });
    let focus = ctx.native_handle(&SharedString::from(id.clone()), cx);
    let control = button(
        gpui::SharedString::from(id.clone()),
        command.label(),
        &ctx.measure,
    )
    .on_click(move |window, cx| act(window, cx));
    let mut control = match command.weight() {
        Weight::Primary => control.primary(),
        Weight::Plain => control.ghost(),
        Weight::Danger => control.danger().ghost(),
    };
    if let Some(focus) = focus {
        control = control.focus_handle(focus);
    }
    ctx.targets
        .track(
            id,
            div()
                .key_context(crate::shell::keys::NATIVE_CONTROL)
                .child(control),
        )
        .into_any_element()
}
