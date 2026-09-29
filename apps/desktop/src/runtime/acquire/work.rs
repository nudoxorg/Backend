//! The worker that adds releases to the library: one thread, one job at a
//! time, in order, each release indexed by the owner with `Session::index`.
//!
//! A job is a release a person asked for (Find, a package page), which goes
//! to the front, or the registry packages a project builds with, which go to
//! the back. The functions here take a session and a source and nothing of
//! the window, so a test drives them against a real owner exactly as the
//! worker does.

use super::Stage;
use crate::core::LocalProjectId;
use crate::host::registry::{Availability, Composition, RegistrySource};
use crate::model::pages::PackageRef;
use crate::model::release::Release;
use backend_client::Session;
use backend_library::browse::{PackageOrigin, PackageRole, ProjectTree};
use backend_library::{ProductText, SurfaceCommand, SurfaceReply};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

/// One thing the worker does.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Job {
    /// A release a person asked for.
    Release(Release),
    /// The registry packages `project` builds with.
    Dependencies(LocalProjectId),
}

/// Where one of a project's packages comes from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Origin {
    /// A registry release, and where its source is on this machine.
    Registry(Availability),
    /// Not a registry release (a git checkout, a vendored path): not added,
    /// in these words.
    Elsewhere(Arc<str>),
}

/// One package a project builds with.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Dependency {
    pub(crate) release: Release,
    /// One of the project's direct dependencies.
    pub(crate) direct: bool,
    pub(crate) origin: Origin,
}

/// What the worker tells the window.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Landed {
    /// A release reached a stage.
    Stage(Release, Stage),
    /// A project's packages were read (in the order they are added), or why not.
    Dependencies(LocalProjectId, Result<Arc<[Dependency]>, Arc<str>>),
}

/// The jobs waiting, and whether a worker is draining them.
#[derive(Default)]
struct Pending {
    jobs: VecDeque<Job>,
    running: bool,
}

/// The job queue a window's additions go through.
pub(crate) struct Queue {
    pending: Mutex<Pending>,
}

impl Queue {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self { pending: Mutex::new(Pending::default()) })
    }

    /// Queues `job` (first when a person asked for it) and starts a worker
    /// when none runs. Returns whether the job was new.
    pub(crate) fn push(self: &Arc<Self>, job: Job, composition: Composition, post: Arc<dyn Fn(Landed) + Send + Sync>) -> bool {
        let start = {
            let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
            if pending.jobs.contains(&job) {
                return false;
            }
            match job {
                Job::Release(_) => pending.jobs.push_front(job),
                Job::Dependencies(_) => pending.jobs.push_back(job),
            }
            !std::mem::replace(&mut pending.running, true)
        };
        if start {
            let queue = Arc::clone(self);
            let spawned = std::thread::Builder::new().name("nudox-acquire".to_owned()).spawn(move || queue.drain(&composition, post.as_ref()));
            if spawned.is_err() {
                self.pending.lock().unwrap_or_else(PoisonError::into_inner).running = false;
            }
        }
        true
    }

    /// Whether a worker is draining the queue.
    pub(crate) fn running(&self) -> bool {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner).running
    }

    fn next(&self) -> Option<Job> {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        let job = pending.jobs.pop_front();
        if job.is_none() {
            pending.running = false;
        }
        job
    }

    fn drain(&self, composition: &Composition, post: &(dyn Fn(Landed) + Send + Sync)) {
        while let Some(job) = self.next() {
            // A panic is that job's failure, never a worker that silently stops.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&job, composition, post)));
            if let Err(panic) = outcome {
                let words: Arc<str> = Arc::from(format!("the work panicked: {}", super::super::offload::describe(panic.as_ref())));
                match job {
                    Job::Release(release) => post(Landed::Stage(release, Stage::Failed(words))),
                    Job::Dependencies(project) => post(Landed::Dependencies(project, Err(words))),
                }
            }
        }
    }
}

fn run(job: &Job, composition: &Composition, post: &(dyn Fn(Landed) + Send + Sync)) {
    match job {
        Job::Release(release) => {
            index_release(composition, release, Listed::Ready, &|stage| post(Landed::Stage(release.clone(), stage)));
        }
        Job::Dependencies(project) => {
            let root = project.path();
            let read = if root.join("Cargo.toml").is_file() {
                Session::connect(&composition.endpoint)
                    .map_err(|error| format!("the index could not be reached: {error}"))
                    .and_then(|mut session| dependencies(&mut session, composition.source.as_ref(), &root))
            } else {
                Err(NOT_CARGO.to_owned())
            };
            match read {
                Ok(found) => {
                    let found: Arc<[Dependency]> = found.into();
                    post(Landed::Dependencies(project.clone(), Ok(Arc::clone(&found))));
                    for dependency in found.iter().filter(|dependency| matches!(dependency.origin, Origin::Registry(_))) {
                        let release = &dependency.release;
                        reads_first();
                        // A package the owner already lists, in any state, was
                        // read once: a relaunch or a project indexed again does
                        // not compile it again (a refusal stays the owner's).
                        index_release(composition, release, Listed::Any, &|stage| post(Landed::Stage(release.clone(), stage)));
                    }
                }
                Err(words) => post(Landed::Dependencies(project.clone(), Err(Arc::from(words)))),
            }
        }
    }
}

/// Lets the window's reads through before the next compile of a project's
/// packages holds the owner: what just landed is read (the Library, the page
/// a person opened) before the next package starts (`runtime::traffic`). A
/// release a person asked for starts at once.
fn reads_first() {
    super::super::traffic::yield_to_reads(std::time::Duration::from_millis(300), std::time::Duration::from_secs(10));
}

/// What a project that is not a Cargo project says about its packages: only a
/// Rust project's packages come from the local cargo cache.
pub(crate) const NOT_CARGO: &str = "only a Rust project's packages are added from the local cargo cache, and this folder has no Cargo.toml";

/// How the owner already lists a release that is therefore not indexed again.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Listed {
    /// Indexed and ready.
    Ready,
    /// In any state, a refusal included.
    Any,
}

/// The packages a project builds with on this machine, in the order they are
/// added (its direct dependencies first, then by distance from its code),
/// from the owner's own resolution of it: `SurfaceCommand::ProjectTree`,
/// which is `cargo metadata --offline --locked` for this host (else its
/// `Cargo.lock`). Packages that build only for other platforms are not in it.
///
/// # Errors
/// The owner's refusal to read the project's tree, in its words.
pub(crate) fn dependencies(session: &mut Session, source: &dyn RegistrySource, root: &Path) -> Result<Vec<Dependency>, String> {
    let text = root.to_str().ok_or_else(|| format!("{} is not UTF-8", root.display()))?;
    let root = ProductText::new(text.to_owned()).map_err(|error| error.to_string())?;
    let tree = match session.surface(SurfaceCommand::ProjectTree { root }).map_err(|error| format!("the index could not read the project's packages: {error}"))? {
        SurfaceReply::ProjectTree(tree) => tree,
        other => return Err(format!("the index answered the project's packages with {:?}", other.id())),
    };
    Ok(ordered(&tree, source))
}

fn ordered(tree: &ProjectTree, source: &dyn RegistrySource) -> Vec<Dependency> {
    let mut packages = tree.packages.iter().collect::<Vec<_>>();
    packages.sort_by_key(|package| (package.role != PackageRole::Direct, package.why.len(), package.name.clone(), package.version.clone()));
    packages
        .into_iter()
        .filter_map(|package| {
            let release = Release::new(&package.name, &package.version).ok()?;
            let origin = match &package.origin {
                PackageOrigin::Registry => Origin::Registry(source.availability(&release)),
                PackageOrigin::Git { url } => Origin::Elsewhere(Arc::from(format!("from git ({url}): only registry releases are added"))),
                PackageOrigin::Vendored { path } => Origin::Elsewhere(Arc::from(format!("vendored at {path}: only registry releases are added"))),
            };
            Some(Dependency { release, direct: package.role == PackageRole::Direct, origin })
        })
        .collect()
}

/// Resolves `release` through the source and has the owner index its tree,
/// posting each stage it reaches; returns the last one. A release the owner
/// already lists as `skip` says is not indexed again.
pub(crate) fn index_release(composition: &Composition, release: &Release, skip: Listed, post: &dyn Fn(Stage)) -> Stage {
    let finish = |stage: Stage| {
        post(stage.clone());
        stage
    };
    let failed = |words: String| finish(Stage::Failed(Arc::from(words)));
    post(Stage::Resolving);
    match composition.source.availability(release) {
        Availability::Unpacked(_) => {}
        Availability::Archive(_) => post(Stage::Unpacking),
        Availability::Download => return failed(format!("{release} is not on this machine; reading it needs a download")),
    }
    let tree = match composition.source.resolve(release) {
        Ok(tree) => tree,
        Err(error) => return failed(error.to_string()),
    };
    let Some(coordinate) = tree.root.to_str() else { return failed(format!("{} is not UTF-8", tree.root.display())) };
    let started = std::time::Instant::now();
    let refusals = composition.refusals.as_deref().map(Refusals::at);
    let mut listed_already = false;
    let indexed = Session::connect(&composition.endpoint).and_then(|mut session| {
        if is_listed(&mut session, coordinate, skip) {
            listed_already = true;
            return Ok(());
        }
        post(Stage::Indexing);
        session.index(coordinate).map(|_| ())
    });
    crate::runtime::trace::span("acquire.index", started, format_args!("{release}"));
    match (indexed, PackageRef::parse(coordinate)) {
        // Listed by an earlier launch: what the owner said then, if it could
        // not finish the compile, still stands.
        (Ok(()), Ok(package)) if listed_already => match refusals.as_ref().and_then(|refusals| refusals.words(coordinate)) {
            Some(words) => finish(Stage::Partial { page: package, words }),
            None => finish(Stage::Added(package)),
        },
        (Ok(()), Ok(package)) => {
            if let Some(refusals) = &refusals {
                refusals.forget(coordinate);
            }
            finish(Stage::Added(package))
        }
        // A compile the owner refused still lists the release, on the names
        // its source declares: it is in the library, and says why it is thin.
        (Err(error), Ok(package))
            if Session::connect(&composition.endpoint).is_ok_and(|mut session| is_listed(&mut session, coordinate, Listed::Any)) =>
        {
            let words: Arc<str> = Arc::from(error.to_string());
            if let Some(refusals) = &refusals {
                refusals.keep(coordinate, &words);
            }
            finish(Stage::Partial { page: package, words })
        }
        (Err(error), _) => failed(format!("the index refused {release}: {error}")),
        (Ok(()), Err(error)) => failed(format!("{coordinate} is not a package address: {error:?}")),
    }
}

/// The owner's words for each release it lists but could not compile, kept
/// beside its workspace: a relaunch reads a release the owner already lists
/// as it was left, thin and why, not as fully added. One small JSON map from
/// the release's source tree to the words; written whole, atomically.
pub(crate) struct Refusals {
    path: std::path::PathBuf,
}

impl Refusals {
    pub(crate) fn at(path: &Path) -> Self {
        Self { path: path.to_path_buf() }
    }

    fn read(&self) -> std::collections::BTreeMap<String, String> {
        std::fs::read(&self.path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
    }

    fn write(&self, map: &std::collections::BTreeMap<String, String>) {
        let Some(parent) = self.path.parent() else { return };
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        let Ok(bytes) = serde_json::to_vec_pretty(map) else { return };
        let staged = self.path.with_extension("json.new");
        if std::fs::write(&staged, bytes).is_ok() {
            let _ = std::fs::rename(&staged, &self.path);
        }
    }

    /// The words kept for `coordinate`.
    pub(crate) fn words(&self, coordinate: &str) -> Option<Arc<str>> {
        self.read().get(coordinate).map(|words| Arc::from(words.as_str()))
    }

    /// Keeps `words` for `coordinate`.
    pub(crate) fn keep(&self, coordinate: &str, words: &str) {
        let mut map = self.read();
        map.insert(coordinate.to_owned(), words.to_owned());
        self.write(&map);
    }

    /// Forgets `coordinate`: its compile finished.
    pub(crate) fn forget(&self, coordinate: &str) {
        let mut map = self.read();
        if map.remove(coordinate).is_some() {
            self.write(&map);
        }
    }
}

/// Whether the owner already lists `coordinate` as `listed` says.
fn is_listed(session: &mut Session, coordinate: &str, listed: Listed) -> bool {
    let Ok(reply) = session.packages() else { return false };
    let backend_library::CommandReply::Packages(snapshot) = reply.reply else { return false };
    snapshot.root.rows().iter().any(|row| row.label == coordinate && (listed == Listed::Any || row.state == backend_library::RowState::Ready))
}
