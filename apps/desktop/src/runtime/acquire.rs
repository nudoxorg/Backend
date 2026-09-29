//! Adding registry releases to the library, through the owner (W-Acquire):
//! a release a person asks for (Find, a package page), and every registry
//! package a project builds with, once the project itself is indexed.
//!
//! Each release resolves to its source tree through the process's
//! [`RegistrySource`](crate::host::registry::RegistrySource), and the owner
//! indexes that tree as a root with `Session::index`, the one command a
//! project reaches it through (`LocalEngineClient::request_index`). A release
//! is not a workspace project, so it does not ride the project lane (which
//! makes what it indexed the active project).
//!
//! One worker ([`work::Queue`]) takes the jobs in order: a person's ask first,
//! a project's packages behind it. The owner may compile each for minutes.
//! Every stage is posted and wakes one UI task; nothing polls. Each release
//! the owner indexes makes the window read its root again, so the Library
//! grows as its packages arrive.

mod work;

pub(crate) use work::{Dependency, Origin, dependencies, index_release};

use super::offload::Asker;
use super::ui_graph::UiRootEntity;
use super::wake::{WakeSender, wake_channel};
use crate::core::LocalProjectId;
use crate::host::registry::Composition;
use crate::model::pages::PackageRef;
use crate::model::release::Release;
use crate::model::{AppSnapshot, ProjectPhase};
use gpui::{App, Global, Task, WeakEntity};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use work::{Job, Landed, Queue};

/// Where adding one release stands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Stage {
    /// Waiting for the worker (another release is being indexed).
    Queued,
    /// Finding its source on this machine.
    Resolving,
    /// Checking and unpacking its registry archive (cargo had not).
    Unpacking,
    /// The owner is indexing its source tree.
    Indexing,
    /// In the library: its page is this package.
    Added(PackageRef),
    /// Not added, in the source's or the owner's words.
    Failed(Arc<str>),
}

impl Stage {
    /// Whether work is still under way (or waiting to be).
    pub(crate) const fn working(&self) -> bool {
        matches!(self, Self::Queued | Self::Resolving | Self::Unpacking | Self::Indexing)
    }
}

/// A project's packages as the worker read them, and how far adding them got.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ProjectPackages {
    /// The owner is being asked which packages the project builds with.
    Reading,
    /// Its packages, and each one's stage, in the order they are added.
    Read(Vec<(Dependency, Option<Stage>)>),
    /// The owner could not say, in its words.
    Refused(Arc<str>),
}

/// One addition and the views waiting on it.
struct Entry {
    stage: Stage,
    askers: Vec<Asker>,
}

/// One project whose packages are being added.
struct Project {
    read: Result<Option<Arc<[Dependency]>>, Arc<str>>,
    askers: Vec<Asker>,
}

/// Every addition this window started.
struct Additions {
    entries: HashMap<Release, Entry>,
    projects: HashMap<LocalProjectId, Project>,
    queue: Arc<Queue>,
    mailbox: Arc<Mutex<Vec<Landed>>>,
    wake: WakeSender,
    /// The one task that lands what the worker posts.
    _drain: Task<()>,
    /// The window root, to read its root again once a release is indexed.
    root: Option<WeakEntity<UiRootEntity>>,
}

impl Global for Additions {}

fn additions(cx: &mut App) -> &mut Additions {
    if cx.try_global::<Additions>().is_none() {
        let (wake, mut receiver) = wake_channel();
        let drain = cx.spawn(async move |cx| {
            while receiver.wait().await.is_some() {
                cx.update(drain);
            }
        });
        cx.set_global(Additions {
            entries: HashMap::new(),
            projects: HashMap::new(),
            queue: Queue::new(),
            mailbox: Arc::default(),
            wake,
            _drain: drain,
            root: None,
        });
    }
    cx.global_mut::<Additions>()
}

/// Queues `job` on the window's worker.
fn push(job: Job, composition: Composition, cx: &mut App) {
    let additions = additions(cx);
    let (mailbox, wake) = (Arc::clone(&additions.mailbox), additions.wake.clone());
    let post: Arc<dyn Fn(Landed) + Send + Sync> = Arc::new(move |landed| {
        mailbox.lock().unwrap_or_else(PoisonError::into_inner).push(landed);
        wake.wake();
    });
    additions.queue.push(job, composition, post);
}

/// Where adding `release` stands, if it was ever asked for; `asker` is told
/// when it moves.
pub(crate) fn stage(release: &Release, asker: Asker, cx: &mut App) -> Option<Stage> {
    cx.try_global::<Additions>()?;
    let entry = cx.global_mut::<Additions>().entries.get_mut(release)?;
    if !entry.askers.contains(&asker) {
        entry.askers.push(asker);
    }
    Some(entry.stage.clone())
}

/// Adds `release` to the library, ahead of any project's packages. Asking
/// again while it is under way, or once it is added, does nothing; asking
/// again after it failed tries again.
pub(crate) fn add(release: Release, root: WeakEntity<UiRootEntity>, cx: &mut App) {
    add_with(release, crate::host::registry::composed(), root, cx);
}

/// [`add`] through `composition` (the process's, or a test's own).
pub(crate) fn add_with(release: Release, composition: Option<Composition>, root: WeakEntity<UiRootEntity>, cx: &mut App) {
    let additions = additions(cx);
    additions.root = Some(root);
    if additions.entries.get(&release).is_some_and(|entry| !matches!(entry.stage, Stage::Failed(_))) {
        return;
    }
    let askers = additions.entries.remove(&release).map(|entry| entry.askers).unwrap_or_default();
    let Some(composition) = composition else {
        additions.entries.insert(release, Entry { stage: Stage::Failed(Arc::from("the index is not open yet")), askers });
        cx.refresh_windows();
        return;
    };
    additions.entries.insert(release.clone(), Entry { stage: Stage::Queued, askers });
    push(Job::Release(release), composition, cx);
    cx.refresh_windows();
}

/// Adds every registry package the projects the owner just indexed build
/// with. A project counts once it went from indexing to ready in this window
/// (a restored or injected ready project is not re-read).
pub(crate) fn follow_indexed_projects(before: Option<&AppSnapshot>, now: &AppSnapshot, root: WeakEntity<UiRootEntity>, cx: &mut App) {
    let Some(before) = before else { return };
    let was_indexing = |project: &LocalProjectId| {
        before.workspace().projects.iter().any(|row| &row.id == project && matches!(row.phase, ProjectPhase::Indexing))
    };
    let indexed = now
        .workspace()
        .projects
        .iter()
        .filter(|row| row.phase == ProjectPhase::Ready && was_indexing(&row.id))
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    for project in indexed {
        add_dependencies_with(project, crate::host::registry::composed(), root.clone(), cx);
    }
}

/// Adds every registry package `project` builds with, through `composition`.
pub(crate) fn add_dependencies_with(project: LocalProjectId, composition: Option<Composition>, root: WeakEntity<UiRootEntity>, cx: &mut App) {
    let additions = additions(cx);
    additions.root = Some(root);
    let askers = additions.projects.remove(&project).map(|project| project.askers).unwrap_or_default();
    let Some(composition) = composition else {
        additions.projects.insert(project, Project { read: Err(Arc::from("the index is not open yet")), askers });
        return;
    };
    additions.projects.insert(project.clone(), Project { read: Ok(None), askers });
    push(Job::Dependencies(project), composition, cx);
}

/// What adding `project`'s packages has come to, if it was ever asked for;
/// `asker` is told when it moves.
pub(crate) fn project_packages(project: &LocalProjectId, asker: Asker, cx: &mut App) -> Option<ProjectPackages> {
    cx.try_global::<Additions>()?;
    let additions = cx.global_mut::<Additions>();
    let entry = additions.projects.get_mut(project)?;
    if !entry.askers.contains(&asker) {
        entry.askers.push(asker);
    }
    Some(match &entry.read {
        Err(words) => ProjectPackages::Refused(Arc::clone(words)),
        Ok(None) => ProjectPackages::Reading,
        Ok(Some(found)) => {
            let found = Arc::clone(found);
            ProjectPackages::Read(found.iter().map(|dependency| (dependency.clone(), additions.entries.get(&dependency.release).map(|entry| entry.stage.clone()))).collect())
        }
    })
}

/// Blocks until the worker has nothing left to do, or `deadline` passes; what
/// it posted lands on the next turn of the UI. A harness holds one input
/// instant with it: the owner may compile for minutes, longer than one quiet
/// wait. Returns whether it finished.
#[cfg(feature = "visual-harness")]
pub(crate) fn await_workers(deadline: std::time::Duration, cx: &App) -> bool {
    let Some(queue) = cx.try_global::<Additions>().map(|additions| Arc::clone(&additions.queue)) else { return true };
    let started = std::time::Instant::now();
    while queue.running() {
        if started.elapsed() > deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    true
}

/// Lands everything the worker posted: the views that asked redraw, and a
/// release the owner indexed makes the window read its root again.
fn drain(cx: &mut App) {
    let Some(additions) = cx.try_global::<Additions>() else { return };
    let landed = std::mem::take(&mut *additions.mailbox.lock().unwrap_or_else(PoisonError::into_inner));
    if landed.is_empty() {
        return;
    }
    let mut told = Vec::new();
    let mut tell = |askers: &[Asker]| {
        for asker in askers {
            if !told.contains(asker) {
                told.push(*asker);
            }
        }
    };
    let mut added = false;
    let additions = cx.global_mut::<Additions>();
    for landed in landed {
        match landed {
            Landed::Stage(release, stage) => {
                added |= matches!(stage, Stage::Added(_));
                let entry = additions.entries.entry(release.clone()).or_insert_with(|| Entry { stage: Stage::Queued, askers: Vec::new() });
                entry.stage = stage;
                tell(&entry.askers);
                for project in additions.projects.values() {
                    if project.read.as_ref().is_ok_and(|read| read.as_ref().is_some_and(|found| found.iter().any(|dependency| dependency.release == release))) {
                        tell(&project.askers);
                    }
                }
            }
            Landed::Dependencies(project, read) => {
                if let Ok(found) = &read {
                    for dependency in found.iter().filter(|dependency| matches!(dependency.origin, Origin::Registry(_))) {
                        let entry = additions.entries.entry(dependency.release.clone()).or_insert_with(|| Entry { stage: Stage::Queued, askers: Vec::new() });
                        if let Stage::Failed(_) = entry.stage {
                            entry.stage = Stage::Queued;
                        }
                    }
                }
                let entry = additions.projects.entry(project).or_insert_with(|| Project { read: Ok(None), askers: Vec::new() });
                entry.read = read.map(Some);
                tell(&entry.askers);
            }
        }
    }
    let root = additions.root.clone();
    for asker in told {
        match asker {
            Asker::View(view) => cx.notify(view),
            Asker::Everyone => cx.refresh_windows(),
        }
    }
    if added && let Some(root) = root {
        let _ = root.update(cx, |root, cx| root.refresh_root(cx));
    }
}
