//! The Library's words for the first minute: what the app is for while
//! nothing is on the shelf, and what an index is doing while it runs.
//!
//! Only reported facts. The owner reports nothing while it compiles a
//! project (one owner loop, one blocking pass), so the only truths this can
//! tell are the ones the window itself holds: the pass was asked for, and
//! how long ago. It never draws a percentage or names a stage the owner did
//! not report.

use crate::core::LocalProjectId;
use crate::model::{AppSnapshot, Note, ProjectPhase, WorkspaceProject};
use crate::navigation::Intent;
use crate::shell::bodies::{Ctx, Leaf};
use crate::shell::focus::{Act, Target};
use crate::shell::kit::{quiet, text};
use crate::shell::reader::Reader;
use facet::controls::{Glyph, KbdVoice, button, kbd};
use facet::data::{Door, Stage, StageState, gem_progress, seam};
use facet::overlay::tooltip::{TipText, content};
use facet::icons::Kind;
use facet::tokens::fluid::EMPTY_GEM;
use facet::tokens::ty;
use facet::Space;
use gpui::{AnyElement, Context, Global, IntoElement as _, ParentElement, SharedString, Styled, Task, div, px};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// How long a running index is said to have run, at a glance: minutes, and
/// "just now" before the first.
const TICK: Duration = Duration::from_secs(60);

/// The steps an index really takes, as far as the window can attest each:
/// the folder was admitted (it saw to that itself), the pass is with the
/// owner (it asked, and no answer has come), and the answer is what makes a
/// project ready. The middle step is the one the owner reports nothing of,
/// so it is drawn as running with no fraction, never as a percentage.
pub(crate) fn stages(phase: ProjectPhase) -> Option<[Stage; 3]> {
    let middle = match phase {
        ProjectPhase::Indexing => StageState::Now,
        ProjectPhase::Cancelling | ProjectPhase::Cancelled => StageState::Stall,
        ProjectPhase::Failed => StageState::Bad,
        ProjectPhase::Ready | ProjectPhase::Missing => return None,
    };
    Some([
        Stage::new("admitted", StageState::Done),
        Stage::new("indexing", middle),
        Stage::new("ready", StageState::Todo),
    ])
}

/// What resting on each step says. A tip is one short line: it does not wrap.
fn tip(phase: ProjectPhase, step: usize) -> TipText {
    let (title, body) = match (step, phase) {
        (0, _) => ("Folder added", "It is on your shelf."),
        (1, ProjectPhase::Failed) => ("Stopped", "Why is written below."),
        (1, ProjectPhase::Cancelling | ProjectPhase::Cancelled) => ("Paused", "Resume it from the project."),
        (1, _) => ("Compiling", "One pass; no finer step is reported."),
        _ => ("Ready to browse", "Pages open when the pass ends."),
    };
    TipText { title: Some(title.into()), body: body.into(), chord: Vec::new() }
}

/// The door each step opens: its tip.
fn door(phase: ProjectPhase) -> Door {
    Door::tip(move |step, measure, window, cx| content(tip(phase, step))(measure, window, cx))
}

/// A project's stone: the kind's own gem, filling by step while its index
/// runs (working), amber if it was paused, cracked coral if it stopped; the
/// plain stone when there is nothing running to show.
pub(crate) fn tile_gem(project: &WorkspaceProject, edge: f32, active: bool, ctx: &Ctx<'_>) -> AnyElement {
    let opacity = if active { 1.0 } else { 0.8 };
    let plain = || facet::paint::gem(Kind::Module).size(edge).opacity(opacity).into_any_element();
    let Some(stages) = stages(project.phase) else { return plain() };
    let id = SharedString::from(format!("project-gem-{}", project.path));
    div()
        .opacity(opacity)
        .child(gem_progress(id, Kind::Module, stages.to_vec(), &ctx.measure).size(edge / ctx.measure.scale()).door(door(project.phase)))
        .into_any_element()
}

/// What the window has to say about this launch, once, at the top of the
/// Library: a session it could not read, an index it set aside. Each is
/// dismissed by the person, and none is written to disk.
pub(crate) fn notes(snapshot: &AppSnapshot, ctx: &mut Ctx<'_>) -> Vec<Leaf> {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let mut leaves = Vec::new();
    for (index, note) in snapshot.workspace().notes.iter().enumerate() {
        let (headline, detail) = match note {
            Note::StateKept { backup, why } => (
                "Your saved layout could not be read, so this launch started fresh.".to_owned(),
                format!("It said: {why}. The file is kept at {backup}; nothing was deleted."),
            ),
            Note::StateUnread { path, why } => (
                "Your saved layout could not be opened, so this launch started fresh.".to_owned(),
                format!("{path} said: {why}. Nudox will not write over it."),
            ),
            Note::LibraryRebuilding { kept_at } => (
                "Your library was built by an earlier version and is being rebuilt.".to_owned(),
                format!("The earlier index is kept at {kept_at}; nothing was deleted."),
            ),
        };
        let headline = ctx.say(headline);
        let detail = ctx.say(detail);
        let id = format!("note-dismiss-{index}");
        let links = ctx.links.clone();
        let held = note.clone();
        let act: Act = Rc::new(move |_, cx| links.dispatch(Intent::DismissNote(held.clone()), cx));
        ctx.targets.push(Target { id: id.clone().into(), label: "Got it".into(), act: Rc::clone(&act), peek: None, source: None });
        let dismiss = ctx.targets.track(id.clone(), button(SharedString::from(id), "Got it", &measure).ghost().on_click(move |window, cx| act(window, cx)));
        leaves.push(Leaf::new(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(measure.space(Space::Roomy))
                .py(measure.space(Space::Base))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w(px(220.0 * measure.scale()))
                        .gap(measure.space(Space::Hair))
                        .child(text(ty::ROW, &measure, palette.ink0).child(headline))
                        .child(text(ty::SMALL, &measure, palette.ink3).child(detail)),
                )
                .child(dismiss),
        ));
    }
    leaves
}

/// The Library with nothing on the shelf: what to do, in one line, with the
/// one thing to press.
pub(crate) fn empty(ctx: &mut Ctx<'_>) -> Leaf {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let lede = ctx.say("Read the code you depend on.");
    let how = ctx.say("Add a project folder. Nudox compiles it and every package it uses, then keeps them together here, ready to browse.");
    let private = ctx.say("Your source stays on this machine.");
    let links = ctx.links.clone();
    let act: Act = {
        let links = links.clone();
        Rc::new(move |_, cx| links.dispatch(Intent::OpenAddProject, cx))
    };
    ctx.targets.push(Target {
        id: "add-folder".into(),
        label: "Add a folder".into(),
        act: Rc::clone(&act),
        peek: None,
        source: None,
    });
    let add = ctx.targets.track(
        "add-folder",
        button("add-folder", "Add a folder", &measure).primary().on_click(move |window, cx| act(window, cx)),
    );
    Leaf::new(
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(measure.space(Space::Gutter))
            .py(measure.space(Space::Chapter))
            .child(facet::paint::gem(Kind::Module).size(f32::from(EMPTY_GEM.at(ctx.wide.fluid_room()))).opacity(0.5))
            .child(text(ty::LEDE, &measure, palette.ink0).text_center().child(lede))
            .child(text(ty::ROW, &measure, palette.ink2).text_center().child(how))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_center()
                    .items_center()
                    .gap(measure.space(Space::Roomy))
                    .child(add)
                    .child(text(ty::SMALL, &measure, palette.ink3).child("or press"))
                    .child(kbd("⌘O", &measure).voice(KbdVoice::Quiet)),
            )
            .child(quiet(private, &measure, palette)),
    )
}

/// What the index holds once a pass has answered, in numbers the owner gave.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Arrival {
    /// Projects on the shelf the owner has answered for.
    pub ready_projects: usize,
    /// Packages in the library.
    pub packages: usize,
    /// Of those, the shelf's own folders.
    pub yours: usize,
    /// Declarations the visible revision holds.
    pub declarations: u64,
    /// Files that published declarations.
    pub files_indexed: u64,
    /// Files the revision selected.
    pub files_discovered: u64,
}

impl Arrival {
    /// What arrived, said so that a project whose dependencies did not arrive
    /// does not look complete.
    pub(crate) fn says(&self) -> Vec<String> {
        let counts = format!("{} declarations from {} of {} files", self.declarations, self.files_indexed, self.files_discovered);
        if self.ready_projects == 0 {
            return vec![counts];
        }
        let plural = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let depended = self.packages.saturating_sub(self.yours);
        if depended == 0 {
            let whose = if self.ready_projects == 1 { "your folder is" } else { "your folders are" };
            vec![
                format!("Only {whose} in the library so far: {}, {counts}.", plural(self.packages, "package", "packages")),
                "The packages it uses appear here once they are indexed.".to_owned(),
            ]
        } else {
            vec![format!(
                "{} in the library: {} yours, {} from what they use. {counts}.",
                plural(self.packages, "package", "packages"),
                self.yours,
                depended
            )]
        }
    }
}

/// The way to add another folder, under the projects already there: the same
/// door the empty Library has, quieter.
pub(crate) fn add_another(ctx: &mut Ctx<'_>) -> Leaf {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let links = ctx.links.clone();
    let act: Act = Rc::new(move |_, cx| links.dispatch(Intent::OpenAddProject, cx));
    ctx.targets.push(Target { id: "add-folder".into(), label: "Add a folder".into(), act: Rc::clone(&act), peek: None, source: None });
    let add = ctx.targets.track(
        "add-folder",
        button("add-folder", "Add a folder", &measure).ghost().glyph(Glyph::Plus).on_click(move |window, cx| act(window, cx)),
    );
    Leaf::new(
        div()
            .flex()
            .flex_wrap()
            .justify_center()
            .items_center()
            .gap(measure.space(Space::Roomy))
            .child(add)
            .child(text(ty::SMALL, &measure, palette.ink3).child("or press"))
            .child(kbd("⌘O", &measure).voice(KbdVoice::Quiet)),
    )
}

/// What each running index is doing, under the projects it belongs to.
pub(crate) fn indexing(snapshot: &AppSnapshot, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) -> Option<Leaf> {
    let running: Vec<_> = snapshot
        .workspace()
        .projects
        .iter()
        .filter(|project| project.phase == ProjectPhase::Indexing)
        .collect();
    let ids: Vec<LocalProjectId> = running.iter().map(|project| project.id.clone()).collect();
    let ages = Ages::observe(&ids, cx);
    if running.is_empty() {
        return None;
    }
    let measure = ctx.measure;
    let palette = ctx.palette;
    let mut block = div().flex().flex_col().items_center().gap(measure.space(Space::Roomy)).py(measure.space(Space::Wide));
    for project in running {
        let headline = ctx.say(format!("Compiling {} and the packages it uses.", project.label));
        let promise = ctx.say(
            "The index reads the whole dependency graph in one pass, so a first index takes a few minutes. Pages open when it finishes.",
        );
        let since = ctx.say(format!("started {}", ago(ages.of(&project.id))));
        // The seam is the strip under the thing being worked on: as wide as
        // its words, never wider than the room.
        let strip = measure.within(px(360.0 * measure.scale()).min(measure.width()));
        let seam_id = SharedString::from(format!("index-seam-{}", project.path));
        block = block.child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(measure.space(Space::Snug))
                .child(text(ty::ROW, &measure, palette.ink0).text_center().child(headline))
                .child(seam(seam_id, stages(project.phase).map_or_else(Vec::new, |steps| steps.to_vec()), &strip).door(door(project.phase)))
                .child(text(ty::SMALL, &measure, palette.ink2).text_center().child(promise))
                .child(text(ty::MONO_SMALL, &measure, palette.ink3).child(since)),
        );
    }
    Some(Leaf::new(block))
}

/// "just now", "1 min ago", "5 min ago", "1 h 5 min ago".
pub(crate) fn ago(elapsed: Duration) -> String {
    let minutes = elapsed.as_secs() / TICK.as_secs();
    match minutes {
        0 => "just now".to_owned(),
        1..=59 => format!("{minutes} min ago"),
        _ => match minutes % 60 {
            0 => format!("{} h ago", minutes / 60),
            rest => format!("{} h {rest} min ago", minutes / 60),
        },
    }
}

/// When the window first drew each running index, on the motion clock
/// (virtual under the harness, so a capture says the same thing every run).
///
/// The owner does not report when a pass began; the window knows when it
/// first showed one. After a relaunch the pass is asked for again and the
/// clock starts again, which is what happened.
#[derive(Default)]
struct Ages {
    first_seen: HashMap<LocalProjectId, Instant>,
    tick: Option<Task<()>>,
}

impl Global for Ages {}

impl Ages {
    /// Notes which indexes are running now, and arms one timer to redraw the
    /// page when the youngest of them next turns a minute older.
    fn observe(running: &[LocalProjectId], cx: &mut Context<Reader>) -> Elapsed {
        let now = facet::motion::now(cx);
        let (elapsed, arm) = {
            let ages = cx.default_global::<Self>();
            ages.first_seen.retain(|id, _| running.contains(id));
            for id in running {
                ages.first_seen.entry(id.clone()).or_insert(now);
            }
            let elapsed = Elapsed {
                ages: ages.first_seen.iter().map(|(id, seen)| (id.clone(), now.saturating_duration_since(*seen))).collect(),
            };
            let arm = ages.tick.is_none().then(|| elapsed.next_minute()).flatten();
            (elapsed, arm)
        };
        if let Some(wait) = arm {
            let tick = cx.spawn(async move |reader, cx| {
                cx.background_executor().timer(wait).await;
                let _ = cx.update_global(|ages: &mut Self, _| ages.tick = None);
                let _ = reader.update(cx, |_, cx| cx.notify());
            });
            cx.default_global::<Self>().tick = Some(tick);
        }
        elapsed
    }
}

/// How long each running index has been on screen.
struct Elapsed {
    ages: HashMap<LocalProjectId, Duration>,
}

impl Elapsed {
    /// How long until the youngest running index turns a minute older.
    fn next_minute(&self) -> Option<Duration> {
        self.ages.values().map(|age| TICK - Duration::from_secs(age.as_secs() % TICK.as_secs())).min()
    }

    fn of(&self, id: &LocalProjectId) -> Duration {
        self.ages.get(id).copied().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_arrived_is_said_so_a_project_without_its_dependencies_does_not_look_complete() {
        let arrival = |ready_projects, packages, yours| Arrival { ready_projects, packages, yours, declarations: 25, files_indexed: 1, files_discovered: 1 };
        assert_eq!(
            arrival(1, 1, 1).says(),
            [
                "Only your folder is in the library so far: 1 package, 25 declarations from 1 of 1 files.",
                "The packages it uses appear here once they are indexed."
            ],
            "one package, and it is the project itself"
        );
        assert_eq!(
            arrival(2, 2, 2).says()[0],
            "Only your folders are in the library so far: 2 packages, 25 declarations from 1 of 1 files."
        );
        assert_eq!(
            arrival(1, 12, 1).says(),
            ["12 packages in the library: 1 yours, 11 from what they use. 25 declarations from 1 of 1 files."],
            "with its dependencies there is nothing to warn about"
        );
        assert_eq!(arrival(0, 3, 0).says(), ["25 declarations from 1 of 1 files"], "nothing of yours answered: just the counts");
    }

    #[test]
    fn the_steps_of_an_index_follow_what_the_owner_answered() {
        let middle = |phase| stages(phase).map(|steps| (steps[0].state, steps[1].state, steps[2].state));
        assert_eq!(middle(ProjectPhase::Indexing), Some((StageState::Done, StageState::Now, StageState::Todo)), "asked, no answer yet: running, no fraction");
        assert_eq!(middle(ProjectPhase::Failed), Some((StageState::Done, StageState::Bad, StageState::Todo)), "the owner refused: the step it stopped on is bad");
        assert_eq!(middle(ProjectPhase::Cancelled), Some((StageState::Done, StageState::Stall, StageState::Todo)), "stopped by the person: waiting, not failed");
        assert_eq!(middle(ProjectPhase::Ready), None, "an answered index has no steps left to show");
        assert!(
            stages(ProjectPhase::Indexing).is_some_and(|steps| steps.iter().all(|step| step.done <= 1.0 && (step.state == StageState::Done || step.done == 0.0))),
            "no step claims a fraction of work the owner never reported"
        );
    }

    #[test]
    fn a_running_index_is_dated_in_minutes_and_hours() {
        let secs = Duration::from_secs;
        assert_eq!(ago(secs(0)), "just now");
        assert_eq!(ago(secs(59)), "just now");
        assert_eq!(ago(secs(60)), "1 min ago");
        assert_eq!(ago(secs(59 * 60 + 59)), "59 min ago");
        assert_eq!(ago(secs(3_600)), "1 h ago");
        assert_eq!(ago(secs(3_600 + 5 * 60)), "1 h 5 min ago");
    }
}
