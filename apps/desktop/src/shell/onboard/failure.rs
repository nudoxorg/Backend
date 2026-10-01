//! A project that did not index: what stopped, in words a person can act on,
//! with the owner's own words one press away, and the three things to do.
//!
//! The owner's refusal is text (`command execution failed: local semantic
//! compilation failed; prior selected semantic generation was preserved:
//! package semantic compilation failed for src/de/mod.rs: Compile { … cause:
//! Lowering(…) }`). [`Cause::of`] reads what it can say with confidence out
//! of it (which file, which language, whether the last good index is still in
//! use) and says nothing it cannot. The owner's exact words are never
//! replaced: the disclosure shows them.

use super::commands::{ProjectCommand, Weight};
use crate::core::LocalProjectId;
use crate::model::{ProjectPhase, WorkspaceProject};
use crate::shell::bodies::{Ctx, Leaf};
use crate::shell::focus::{Act, Target};
use crate::shell::kit::text;
use crate::shell::reader::Reader;
use facet::Space;
use facet::controls::button;
use facet::icons::{Icon, IconSize, ui};
use facet::tokens::ty;
use gpui::{
    BorrowAppContext as _, Context, Global, InteractiveElement as _, IntoElement as _,
    ParentElement, StatefulInteractiveElement as _, Styled, div, px,
};
use std::collections::HashSet;
use std::rc::Rc;

/// What stopped an index, as far as the owner's words say.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Cause {
    /// The compiler could not finish reading a file.
    Compile {
        /// The file, when the owner named one.
        file: Option<String>,
        /// The language, when the owner named it as unavailable.
        unavailable: Option<String>,
        /// Whether the last good index of this project is still in use.
        kept_last: bool,
    },
    /// The index could not store what it read.
    Store,
    /// Nudox could not reach its index.
    Reach,
    /// Anything else the owner said.
    Other,
}

impl Cause {
    /// Reads the owner's words.
    pub(crate) fn of(owner_words: &str) -> Self {
        let kept_last = owner_words.contains("prior selected semantic generation was preserved");
        if owner_words.contains("semantic compilation failed") {
            let file = owner_words
                .split_once("compilation failed for ")
                .and_then(|(_, rest)| rest.split_once(':'))
                .map(|(file, _)| file.trim().to_owned())
                .filter(|file| !file.is_empty() && !file.contains(' '));
            let unavailable = owner_words
                .split_once("Unavailable { language: ")
                .and_then(|(_, rest)| rest.split_once(','))
                .map(|(language, _)| language.trim().to_owned());
            return Self::Compile {
                file,
                unavailable,
                kept_last,
            };
        }
        if owner_words.contains("queue builtin intent")
            || owner_words.contains("commit product source intent")
        {
            return Self::Store;
        }
        if owner_words.contains("reach the local service")
            || owner_words.contains("could not start")
            || owner_words.contains("connection")
        {
            return Self::Reach;
        }
        Self::Other
    }

    /// What a person reads: one sentence, then, when it is true, that the
    /// last good index still stands.
    pub(crate) fn says(&self) -> Vec<String> {
        match self {
            Self::Compile {
                file,
                unavailable,
                kept_last,
            } => {
                let mut lines = vec![match (unavailable, file) {
                    (Some(language), _) => format!(
                        "Nudox cannot compile {language} here: its {language} toolchain is not available to the index."
                    ),
                    (None, Some(file)) => format!("The compiler could not finish reading {file}."),
                    (None, None) => {
                        "The compiler could not finish reading this project.".to_owned()
                    }
                }];
                // Rust is found where people install it (`host::toolchain`);
                // unavailable means none was, and the fix is theirs to make.
                if unavailable.as_deref() == Some("Rust") {
                    lines.push("Install Rust with rustup (rustup.rs) or Homebrew (brew install rust), then quit and reopen Nudox.".to_owned());
                }
                if *kept_last {
                    lines.push("The last good index of this project is still in use.".to_owned());
                }
                lines
            }
            Self::Store => vec!["The index could not store what it read.".to_owned()],
            Self::Reach => vec!["Nudox could not reach its index.".to_owned()],
            Self::Other => vec!["The index stopped before it finished.".to_owned()],
        }
    }
}

/// Which failed projects show the owner's words.
#[derive(Default)]
struct Disclosed(HashSet<LocalProjectId>);

impl Global for Disclosed {}

/// The card for each project that stopped, paused, or whose folder is gone.
pub(crate) fn stopped(
    projects: &[WorkspaceProject],
    ctx: &mut Ctx<'_>,
    cx: &mut Context<Reader>,
) -> Option<Leaf> {
    let cards: Vec<_> = projects
        .iter()
        .filter(|project| {
            matches!(
                project.phase,
                ProjectPhase::Failed | ProjectPhase::Cancelled | ProjectPhase::Missing
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
    let (headline, lines, owner_words) = match project.phase {
        ProjectPhase::Failed => {
            let owner = project.error.as_deref().unwrap_or_default();
            (
                format!("{} stopped.", project.label),
                Cause::of(owner).says(),
                (!owner.is_empty()).then(|| owner.to_owned()),
            )
        }
        ProjectPhase::Cancelled => (
            format!("{} is paused.", project.label),
            vec!["Its index was stopped before it finished.".to_owned()],
            None,
        ),
        _ => (
            format!("{} is not where it was.", project.label),
            vec![format!("Nudox looked for it at {}.", project.path)],
            None,
        ),
    };
    let headline = ctx.say(headline);
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
            .child(text(ty::ROW, &measure, palette.ink0).child(headline)),
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
    for command in ProjectCommand::for_phase(project.phase, false)
        .into_iter()
        .filter(|command| *command != ProjectCommand::Activate)
    {
        actions = actions.child(act(
            ctx,
            format!("{command:?}-{}", project.path).to_lowercase(),
            command,
            &id,
        ));
    }
    stack = stack.child(actions);
    if let Some(words) = owner_words {
        let shown = cx.default_global::<Disclosed>().0.contains(&id);
        let reader = cx.entity();
        let toggle = id.clone();
        let label = ctx.say(if shown {
            "Hide the index's own words"
        } else {
            "The index's own words"
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
        ctx.targets.push(Target {
            id: door.clone().into(),
            label: label.clone(),
            act: Rc::clone(&act),
            peek: None,
            source: None,
        });
        stack = stack.child(
            ctx.targets.track(
                door.clone(),
                div()
                    .id(gpui::SharedString::from(door))
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .justify_center()
                    .min_h(px(24.0 * measure.scale()))
                    .px(measure.space(Space::Roomy))
                    .hover(|style| style.bg(palette.tint))
                    .child(text(ty::SMALL, &measure, palette.ink2).child(label))
                    .on_click(move |_, window, cx| act(window, cx)),
            ),
        );
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
) -> gpui::AnyElement {
    let links = ctx.links.clone();
    let intent = command.intent(project);
    let act: Act = Rc::new(move |_, cx| links.dispatch(intent.clone(), cx));
    ctx.targets.push(Target {
        id: id.clone().into(),
        label: command.label().into(),
        act: Rc::clone(&act),
        peek: None,
        source: None,
    });
    let control = button(
        gpui::SharedString::from(id.clone()),
        command.label(),
        &ctx.measure,
    )
    .on_click(move |window, cx| act(window, cx));
    let control = match command.weight() {
        Weight::Primary => control.primary(),
        Weight::Plain => control.ghost(),
        Weight::Danger => control.danger().ghost(),
    };
    ctx.targets.track(id, control).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_owners_real_refusals_are_read_for_what_they_say() {
        let rust = "command execution failed: local semantic compilation failed; prior selected semantic generation was preserved: \
                    package semantic compilation failed for src/de/mod.rs: Compile { attempted: CompilerAttempt { source: \
                    SourceAuthority { identity: ContentId(0e55), byte_len: 83879 } }, cause: Lowering(LoweringCause(RustGenericParameter)) }";
        assert_eq!(
            Cause::of(rust),
            Cause::Compile {
                file: Some("src/de/mod.rs".to_owned()),
                unavailable: None,
                kept_last: true
            }
        );
        assert_eq!(
            Cause::of(rust).says(),
            [
                "The compiler could not finish reading src/de/mod.rs.",
                "The last good index of this project is still in use."
            ]
        );
        let go = "command execution failed: local semantic compilation failed; prior selected semantic generation was preserved: \
                  package semantic compilation failed for bool.go: Unavailable { language: Go, stage: LowerIr }";
        assert_eq!(
            Cause::of(go).says(),
            [
                "Nudox cannot compile Go here: its Go toolchain is not available to the index.",
                "The last good index of this project is still in use."
            ]
        );
        assert_eq!(
            Cause::of("command execution failed: queue builtin intent: Queue(Bytes)"),
            Cause::Store
        );
        assert_eq!(
            Cause::of("reach the local service at /tmp/x.sock: refused"),
            Cause::Reach
        );
        assert_eq!(
            Cause::of("something nobody has seen"),
            Cause::Other,
            "what it cannot read it does not pretend to"
        );
    }
}
