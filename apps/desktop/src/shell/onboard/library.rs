//! The Library's words for the first minute: what the app is for while
//! nothing is on the shelf, and what an index is doing while it runs.
//!
//! Only reported facts. The owner reports nothing while it compiles a
//! project (one owner loop, one blocking pass), so the only truths this can
//! tell are the ones the window itself holds: the pass was asked for, and
//! how long ago. It never draws a percentage or names a stage the owner did
//! not report. Once the project is answered for, the packages it builds with
//! are added one by one (`runtime::acquire`), and each one's own stage is
//! drawn: which is being indexed, how many are in, and each that could not
//! be added, named with its reason.

use crate::core::LocalProjectId;
use crate::model::{AppSnapshot, Note, ProjectPhase, WorkspaceProject};
use crate::navigation::Intent;
use crate::runtime::acquire::{NOT_CARGO, Origin, ProjectPackages, Stage as Adding};
use crate::runtime::offload::Asker;
use crate::shell::bodies::{Ctx, Leaf};
use crate::shell::focus::{Act, Target};
use crate::shell::kit::{quiet, text};
use crate::shell::reader::Reader;
use facet::Space;
use facet::controls::{Glyph, KbdVoice, button, kbd};
use facet::data::{Door, Stage, StageState, gem_progress, seam};
use facet::icons::Kind;
use facet::overlay::tooltip::{TipText, content};
use facet::tokens::fluid::EMPTY_GEM;
use facet::tokens::ty;
use gpui::{
    AnyElement, Context, Global, IntoElement as _, ParentElement, SharedString, Styled, Task, div,
    px,
};
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
        (1, ProjectPhase::Cancelling | ProjectPhase::Cancelled) => {
            ("Paused", "Resume it from the project.")
        }
        (1, _) => ("Compiling", "One pass; no finer step is reported."),
        _ => ("Ready to browse", "Pages open when the pass ends."),
    };
    TipText {
        title: Some(title.into()),
        body: body.into(),
        chord: Vec::new(),
    }
}

/// The door each step opens: its tip.
fn door(phase: ProjectPhase) -> Door {
    Door::tip(move |step, measure, window, cx| content(tip(phase, step))(measure, window, cx))
}

/// A project's stone: the kind's own gem, filling by step while its index
/// runs (working), amber if it was paused, cracked coral if it stopped; the
/// plain stone when there is nothing running to show.
pub(crate) fn tile_gem(
    project: &WorkspaceProject,
    edge: f32,
    active: bool,
    ctx: &Ctx<'_>,
) -> AnyElement {
    let opacity = if active { 1.0 } else { 0.8 };
    let plain = || {
        facet::paint::gem(Kind::Module)
            .size(edge)
            .opacity(opacity)
            .into_any_element()
    };
    let Some(stages) = stages(project.phase) else {
        return plain();
    };
    let id = SharedString::from(format!("project-gem-{}", project.path));
    div()
        .opacity(opacity)
        .child(
            gem_progress(id, Kind::Module, stages.to_vec(), &ctx.measure)
                .size(edge / ctx.measure.scale())
                .door(door(project.phase)),
        )
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
        ctx.targets.push(Target {
            id: id.clone().into(),
            label: "Got it".into(),
            act: Rc::clone(&act),
            peek: None,
            source: None,
        });
        let dismiss = ctx.targets.track(
            id.clone(),
            button(SharedString::from(id), "Got it", &measure)
                .ghost()
                .on_click(move |window, cx| act(window, cx)),
        );
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
    // Which Rust the index compiles with, found where people install it (a
    // Finder launch names none): said before anything is added, so a missing
    // one is not first met as every package refused.
    let rust = crate::host::toolchain::report().map(|rust| {
        let missing = matches!(rust, crate::host::toolchain::Rust::Missing { .. });
        let words = match &rust {
            crate::host::toolchain::Rust::Found { .. } => {
                format!("Compiles with {}.", rust.words())
            }
            crate::host::toolchain::Rust::Missing { .. } => rust.words(),
        };
        (ctx.say(words), missing)
    });
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
        button("add-folder", "Add a folder", &measure)
            .primary()
            .on_click(move |window, cx| act(window, cx)),
    );
    Leaf::new(
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(measure.space(Space::Gutter))
            .py(measure.space(Space::Chapter))
            .child(
                facet::paint::gem(Kind::Module)
                    .size(f32::from(EMPTY_GEM.at(ctx.wide.fluid_room())))
                    .opacity(0.5),
            )
            .child(
                text(ty::LEDE, &measure, palette.ink0)
                    .text_center()
                    .child(lede),
            )
            .child(
                text(ty::ROW, &measure, palette.ink2)
                    .text_center()
                    .child(how),
            )
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
            .child(quiet(private, &measure, palette))
            .children(rust.map(|(words, missing)| {
                if missing {
                    text(ty::ROW, &measure, palette.ink1)
                        .text_center()
                        .min_w(px(0.0))
                        .child(words)
                        .into_any_element()
                } else {
                    quiet(words, &measure, palette)
                        .text_center()
                        .min_w(px(0.0))
                        .into_any_element()
                }
            })),
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
        let counts = format!(
            "{} declarations from {} of {} files",
            self.declarations, self.files_indexed, self.files_discovered
        );
        if self.ready_projects == 0 {
            return vec![counts];
        }
        let plural =
            |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let depended = self.packages.saturating_sub(self.yours);
        if depended == 0 {
            let whose = if self.ready_projects == 1 {
                "your folder is"
            } else {
                "your folders are"
            };
            vec![
                format!(
                    "Only {whose} in the library so far: {}, {counts}.",
                    plural(self.packages, "package", "packages")
                ),
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
    ctx.targets.push(Target {
        id: "add-folder".into(),
        label: "Add a folder".into(),
        act: Rc::clone(&act),
        peek: None,
        source: None,
    });
    let add = ctx.targets.track(
        "add-folder",
        button("add-folder", "Add a folder", &measure)
            .ghost()
            .glyph(Glyph::Plus)
            .on_click(move |window, cx| act(window, cx)),
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

/// What each running index is doing, under the projects it belongs to, and
/// how adding the packages each answered project builds with is going.
pub(crate) fn indexing(
    snapshot: &AppSnapshot,
    ctx: &mut Ctx<'_>,
    cx: &mut Context<Reader>,
) -> Option<Leaf> {
    let running: Vec<_> = snapshot
        .workspace()
        .projects
        .iter()
        .filter(|project| project.phase == ProjectPhase::Indexing)
        .collect();
    let ids: Vec<LocalProjectId> = running.iter().map(|project| project.id.clone()).collect();
    let ages = Ages::observe(&ids, cx);
    let view = cx.entity_id();
    let additions = snapshot
        .workspace()
        .projects
        .iter()
        .filter(|project| project.phase != ProjectPhase::Indexing)
        .filter_map(|project| {
            let packages =
                crate::runtime::acquire::project_packages(&project.id, Asker::View(view), cx)?;
            Some((
                project.path.clone(),
                PackageWords::of(&project.label, &packages),
            ))
        })
        .collect::<Vec<_>>();
    if running.is_empty() && additions.is_empty() {
        return None;
    }
    let measure = ctx.measure;
    let palette = ctx.palette;
    let mut block = div()
        .flex()
        .flex_col()
        .items_center()
        .gap(measure.space(Space::Roomy))
        .py(measure.space(Space::Wide));
    for (path, words) in additions {
        block = block.child(package_block(&path, &words, ctx));
    }
    for project in running {
        let headline = ctx.say(format!("Compiling {}.", project.label));
        let promise = ctx.say(
            "Then each package it uses is indexed from your cargo cache, one at a time. A first install takes a few minutes.",
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
                .child(
                    text(ty::ROW, &measure, palette.ink0)
                        .text_center()
                        .child(headline),
                )
                .child(
                    seam(
                        seam_id,
                        stages(project.phase).map_or_else(Vec::new, |steps| steps.to_vec()),
                        &strip,
                    )
                    .door(door(project.phase)),
                )
                .child(
                    text(ty::SMALL, &measure, palette.ink2)
                        .text_center()
                        .child(promise),
                )
                .child(text(ty::MONO_SMALL, &measure, palette.ink3).child(since)),
        );
    }
    Some(Leaf::new(block))
}

/// How adding one project's packages is going, in the words the Library
/// draws, and one seam step per package.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PackageWords {
    /// What is happening, or what came of it.
    pub headline: String,
    /// The package being worked on now, and which of how many it is (also
    /// in the headline, which says it on one line of full ink).
    pub now: Option<String>,
    /// One step per package, in the order they are added: its name and state.
    pub steps: Vec<(String, StageState, String)>,
    /// Each package that could not be added, with its reason.
    pub refused: Vec<String>,
    /// Each package in the library whose compile the owner could not finish
    /// (its names come from its source alone), with the reason.
    pub thin: Vec<String>,
}

/// The longest reason drawn for a package that could not be added; the
/// owner's full words are longer (its step's tip holds them).
const REASON: usize = 160;

impl PackageWords {
    /// The words for `packages`, the packages of the project named `label`.
    pub(crate) fn of(label: &str, packages: &ProjectPackages) -> Self {
        let plain = |headline: String| Self {
            headline,
            now: None,
            steps: Vec::new(),
            refused: Vec::new(),
            thin: Vec::new(),
        };
        let found = match packages {
            ProjectPackages::Reading => {
                return plain(format!("Reading the packages {label} uses."));
            }
            ProjectPackages::Refused(words) if words.as_ref() == NOT_CARGO => {
                return plain(format!(
                    "Only a Rust project's packages are added for now, and {label} has no Cargo.toml."
                ));
            }
            ProjectPackages::Refused(words) => {
                return plain(format!(
                    "The packages {label} uses could not be read: {words}"
                ));
            }
            ProjectPackages::Read(found) => found,
        };
        let total = found.len();
        if total == 0 {
            return plain(format!("{label} uses no registry packages."));
        }
        let Some(progress) = packages.progress() else {
            return plain(format!("Reading the packages {label} uses."));
        };
        let added = progress.landed;
        let now = progress.now.as_ref().map(|(release, stage, at)| {
            let doing = match stage {
                Adding::Resolving => "finding",
                Adding::Unpacking => "unpacking",
                _ => "indexing",
            };
            format!("{doing} {release} ({at} of {total})")
        });
        let mut steps = Vec::with_capacity(total);
        let (mut refused, mut thin) = (Vec::new(), Vec::new());
        for (dependency, stage) in found {
            let release = dependency.release.to_string();
            let (state, tip) = match (&dependency.origin, stage) {
                (Origin::Elsewhere(why), _) => {
                    refused.push(format!("{release}: {why}"));
                    (StageState::Bad, why.to_string())
                }
                (Origin::Registry(_), Some(Adding::Added(_))) => {
                    (StageState::Done, "In the library.".to_owned())
                }
                (Origin::Registry(_), Some(Adding::Partial { words, .. })) => {
                    let why = super::failure::Cause::of(words)
                        .says()
                        .into_iter()
                        .next()
                        .unwrap_or_default();
                    let why = lowercase_first(why.trim_end_matches('.'));
                    thin.push(format!(
                        "{release}: {why}, so its names come from its source alone."
                    ));
                    (
                        StageState::Stall,
                        format!("In the library from its source alone: {why}."),
                    )
                }
                (Origin::Registry(_), Some(Adding::Failed(why))) => {
                    refused.push(format!("{release}: {}", clip(why)));
                    (StageState::Bad, why.to_string())
                }
                (
                    Origin::Registry(_),
                    Some(working @ (Adding::Resolving | Adding::Unpacking | Adding::Indexing)),
                ) => {
                    let doing = match working {
                        Adding::Resolving => "finding",
                        Adding::Unpacking => "unpacking",
                        _ => "indexing",
                    };
                    (StageState::Now, format!("{} now.", capitalized(doing)))
                }
                (Origin::Registry(_), Some(Adding::Queued) | None) => {
                    (StageState::Todo, "Waiting its turn.".to_owned())
                }
            };
            steps.push((release, state, tip));
        }
        let working = !progress.done;
        let packages = |n: usize| {
            if n == 1 {
                "package".to_owned()
            } else {
                format!("{n} packages")
            }
        };
        let headline = if working {
            match &now {
                Some(now) => format!(
                    "Adding the {} {label} uses: {added} in the library, {now}.",
                    packages(total)
                ),
                None => format!(
                    "Adding the {} {label} uses: {added} in the library so far.",
                    packages(total)
                ),
            }
        } else if !refused.is_empty() {
            format!(
                "{added} of the {} {label} uses are in the library; {} could not be added:",
                packages(total),
                refused.len()
            )
        } else if !thin.is_empty() {
            let all = if total == 1 {
                format!("The package {label} uses is in the library")
            } else {
                format!("All {total} packages {label} uses are in the library")
            };
            format!(
                "{all}; the compiler could not finish {}:",
                if thin.len() == 1 {
                    "one".to_owned()
                } else {
                    thin.len().to_string()
                }
            )
        } else if total == 1 {
            format!("The package {label} uses is in the library.")
        } else {
            format!("All {total} packages {label} uses are in the library.")
        };
        Self {
            headline,
            now,
            steps,
            refused,
            thin,
        }
    }
}

/// `why`'s first line, at most [`REASON`] characters.
fn clip(why: &str) -> String {
    let line = why.lines().next().unwrap_or_default();
    if line.chars().count() <= REASON {
        return line.to_owned();
    }
    let mut clipped = line.chars().take(REASON).collect::<String>();
    clipped.push('…');
    clipped
}

fn lowercase_first(words: &str) -> String {
    let mut chars = words.chars();
    chars
        .next()
        .map(|first| first.to_lowercase().chain(chars).collect())
        .unwrap_or_default()
}

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// One project's packages: the headline, a seam with one step per package
/// (its tip names the package and its state), the one being worked on, and
/// each that could not be added, named.
fn package_block(path: &str, words: &PackageWords, ctx: &mut Ctx<'_>) -> AnyElement {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let headline = ctx.say(words.headline.clone());
    let mut block = div()
        .flex()
        .flex_col()
        .items_center()
        .gap(measure.space(Space::Snug))
        .child(
            text(ty::ROW, &measure, palette.ink0)
                .text_center()
                .min_w(px(0.0))
                .child(headline),
        );
    if !words.steps.is_empty()
        && (words.now.is_some()
            || words
                .steps
                .iter()
                .any(|(_, state, _)| *state == StageState::Todo))
    {
        let strip = measure.within(px(360.0 * measure.scale()).min(measure.width()));
        let stages = words
            .steps
            .iter()
            .map(|(name, state, _)| Stage::new(name.clone(), *state))
            .collect::<Vec<_>>();
        let tips = words
            .steps
            .iter()
            .map(|(name, _, tip)| {
                (
                    SharedString::from(name.clone()),
                    SharedString::from(tip.clone()),
                )
            })
            .collect::<Rc<[_]>>();
        let door = Door::tip(move |step, measure, window, cx| {
            let (title, body) = tips.get(step).cloned().unwrap_or_default();
            content(TipText {
                title: Some(title),
                body,
                chord: Vec::new(),
            })(measure, window, cx)
        });
        block = block.child(
            seam(
                SharedString::from(format!("package-seam-{path}")),
                stages,
                &strip,
            )
            .door(door),
        );
    }
    for line in words.refused.iter().chain(&words.thin) {
        let line = ctx.say(line.clone());
        block = block.child(
            text(ty::SMALL, &measure, palette.ink2)
                .text_center()
                .min_w(px(0.0))
                .child(line),
        );
    }
    block.into_any_element()
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
                ages: ages
                    .first_seen
                    .iter()
                    .map(|(id, seen)| (id.clone(), now.saturating_duration_since(*seen)))
                    .collect(),
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
        self.ages
            .values()
            .map(|age| TICK - Duration::from_secs(age.as_secs() % TICK.as_secs()))
            .min()
    }

    fn of(&self, id: &LocalProjectId) -> Duration {
        self.ages.get(id).copied().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::acquire::Dependency;
    use std::sync::Arc;

    #[test]
    fn what_arrived_is_said_so_a_project_without_its_dependencies_does_not_look_complete() {
        let arrival = |ready_projects, packages, yours| Arrival {
            ready_projects,
            packages,
            yours,
            declarations: 25,
            files_indexed: 1,
            files_discovered: 1,
        };
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
            [
                "12 packages in the library: 1 yours, 11 from what they use. 25 declarations from 1 of 1 files."
            ],
            "with its dependencies there is nothing to warn about"
        );
        assert_eq!(
            arrival(0, 3, 0).says(),
            ["25 declarations from 1 of 1 files"],
            "nothing of yours answered: just the counts"
        );
    }

    #[test]
    fn the_steps_of_an_index_follow_what_the_owner_answered() {
        let middle =
            |phase| stages(phase).map(|steps| (steps[0].state, steps[1].state, steps[2].state));
        assert_eq!(
            middle(ProjectPhase::Indexing),
            Some((StageState::Done, StageState::Now, StageState::Todo)),
            "asked, no answer yet: running, no fraction"
        );
        assert_eq!(
            middle(ProjectPhase::Failed),
            Some((StageState::Done, StageState::Bad, StageState::Todo)),
            "the owner refused: the step it stopped on is bad"
        );
        assert_eq!(
            middle(ProjectPhase::Cancelled),
            Some((StageState::Done, StageState::Stall, StageState::Todo)),
            "stopped by the person: waiting, not failed"
        );
        assert_eq!(
            middle(ProjectPhase::Ready),
            None,
            "an answered index has no steps left to show"
        );
        assert!(
            stages(ProjectPhase::Indexing).is_some_and(|steps| steps.iter().all(|step| step.done
                <= 1.0
                && (step.state == StageState::Done || step.done == 0.0))),
            "no step claims a fraction of work the owner never reported"
        );
    }

    fn dependency(name: &str, version: &str, origin: Origin) -> Dependency {
        Dependency {
            release: crate::model::release::Release::new(name, version).expect("release"),
            direct: false,
            origin,
        }
    }

    fn cached() -> Origin {
        Origin::Registry(crate::model::release::Availability::Unpacked(
            std::path::PathBuf::from("/cache"),
        ))
    }

    #[test]
    fn a_projects_packages_are_said_one_by_one_and_each_that_could_not_be_added_is_named() {
        let page = crate::model::pages::PackageRef::parse("/cache/toml-0.8.23").expect("page");
        let reading = PackageWords::of("toml_pin", &ProjectPackages::Reading);
        assert_eq!(reading.headline, "Reading the packages toml_pin uses.");
        let found = vec![
            (
                dependency("toml", "0.8.23", cached()),
                Some(Adding::Added(page.clone())),
            ),
            (
                dependency("toml_edit", "0.22.27", cached()),
                Some(Adding::Indexing),
            ),
            (
                dependency("winnow", "0.7.15", cached()),
                Some(Adding::Queued),
            ),
            (
                dependency(
                    "hashbrown",
                    "0.17.1",
                    Origin::Registry(crate::model::release::Availability::Download),
                ),
                Some(Adding::Failed(Arc::from(
                    "hashbrown 0.17.1 is not on this machine; reading it needs a download",
                ))),
            ),
            (
                dependency(
                    "forked",
                    "0.1.0",
                    Origin::Elsewhere(Arc::from(
                        "from git (https://example.test/forked): only registry releases are added",
                    )),
                ),
                None,
            ),
        ];
        let working = PackageWords::of("toml_pin", &ProjectPackages::Read(found.clone()));
        assert_eq!(
            working.headline,
            "Adding the 5 packages toml_pin uses: 1 in the library, indexing toml_edit 0.22.27 (2 of 5).",
            "one line says how many there are, how many are in, and which is being indexed"
        );
        assert_eq!(
            working.now.as_deref(),
            Some("indexing toml_edit 0.22.27 (2 of 5)"),
            "the one being indexed, and which of how many"
        );
        assert_eq!(
            working
                .steps
                .iter()
                .map(|(name, state, _)| (name.as_str(), *state))
                .collect::<Vec<_>>(),
            [
                ("toml 0.8.23", StageState::Done),
                ("toml_edit 0.22.27", StageState::Now),
                ("winnow 0.7.15", StageState::Todo),
                ("hashbrown 0.17.1", StageState::Bad),
                ("forked 0.1.0", StageState::Bad)
            ],
            "one step per package, each in its own state"
        );
        assert_eq!(
            working.refused,
            [
                "hashbrown 0.17.1: hashbrown 0.17.1 is not on this machine; reading it needs a download",
                "forked 0.1.0: from git (https://example.test/forked): only registry releases are added"
            ],
            "a package that could not be added is named with its reason, never skipped"
        );
        let mut settled = found;
        settled[1].1 = Some(Adding::Added(page.clone()));
        settled[2].1 = Some(Adding::Added(page.clone()));
        let done = PackageWords::of("toml_pin", &ProjectPackages::Read(settled.clone()));
        assert_eq!(
            done.headline,
            "3 of the 5 packages toml_pin uses are in the library; 2 could not be added:"
        );
        assert_eq!(done.now, None);
        let all = PackageWords::of("toml_pin", &ProjectPackages::Read(settled[..3].to_vec()));
        assert_eq!(
            all.headline,
            "All 3 packages toml_pin uses are in the library."
        );
        assert!(all.refused.is_empty());
        // A compile the owner could not finish is in the library, on its
        // source's names, and says why in a person's words.
        let mut thin = settled[..3].to_vec();
        thin[1].1 = Some(Adding::Partial {
            page: page.clone(),
            words: Arc::from(
                "protocol: command execution failed: local semantic compilation failed; prior selected semantic generation was preserved: package semantic compilation failed for src/alloc.rs: Compile { attempted: …, cause: Fragment(Prepare) }",
            ),
        });
        let thin = PackageWords::of("toml_pin", &ProjectPackages::Read(thin));
        assert_eq!(
            thin.headline,
            "All 3 packages toml_pin uses are in the library; the compiler could not finish one:"
        );
        assert_eq!(
            thin.thin,
            [
                "toml_edit 0.22.27: the compiler could not finish reading src/alloc.rs, so its names come from its source alone."
            ]
        );
        assert_eq!(thin.steps[1].1, StageState::Stall);
        let not_cargo = PackageWords::of("site", &ProjectPackages::Refused(Arc::from(NOT_CARGO)));
        assert_eq!(
            not_cargo.headline,
            "Only a Rust project's packages are added for now, and site has no Cargo.toml."
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
