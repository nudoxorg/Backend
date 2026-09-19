//! Durable intents in flight: adding a project, and taking one off the shelf.
//! A job exists from the moment the reader asks until the shelf proves it done.
//! Failures stay on the row that failed rather than vanishing into a toast.
//!
//! The shelf projection alone cannot show a project that has been requested but
//! has no row yet, and it cannot explain a project that failed to index, because
//! neither fact is in the view root. This store holds exactly those two facts
//! and merges them into the shelf, which is why `+ Add` produces a visible row
//! on the very next frame instead of a spinner and a hope.

use super::events::JobsEvent;
use super::service::{Endpoint, Outcome, Request};
use crate::presentation::fault::{headline, wrong_shape};
use crate::presentation::project;
use backend_present::{Coordinate, Fault, Identity, Operand, Readiness, Shelf};
use gpui::AppContext as _;
use gpui::{Context, EventEmitter, Task};

/// What a job is doing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum JobKind {
    /// Compile, publish, and index a project or package.
    Index,
    /// Take a project or package off the shelf.
    Remove,
}

impl JobKind {
    /// Returns the present-tense verb shown while the job runs.
    pub(crate) const fn verb(self) -> &'static str {
        match self {
            Self::Index => "Indexing",
            Self::Remove => "Removing",
        }
    }
}

/// How far along a job is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum JobState {
    /// The request is on the wire.
    Submitted,
    /// The service accepted the intent; the shelf will show it.
    Accepted,
    /// The service refused; the fault says why and what to do.
    Failed(Box<Fault>),
}

/// One durable intent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Job {
    coordinate: String,
    kind: JobKind,
    state: JobState,
}

impl Job {
    /// Returns how far along the job is.
    pub(crate) const fn state(&self) -> &JobState {
        &self.state
    }

    /// Returns the one-line label shown in the status bar.
    pub(crate) fn line(&self) -> String {
        let identity = Identity::parse(&self.coordinate);
        let name = identity.name().to_owned();
        match &self.state {
            JobState::Submitted | JobState::Accepted => format!("{} {name}…", self.kind.verb()),
            JobState::Failed(fault) => format!("{name}: {}", headline(fault)),
        }
    }
}

/// Every intent this window has submitted and not yet seen resolved.
pub(crate) struct JobsStore {
    endpoint: Endpoint,
    jobs: Vec<Job>,
    running: Vec<Task<()>>,
}

impl EventEmitter<JobsEvent> for JobsStore {}

impl JobsStore {
    /// Creates an empty job list bound to one endpoint.
    pub(crate) const fn new(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            jobs: Vec::new(),
            running: Vec::new(),
        }
    }

    /// Returns every coordinate this window has asked to index, failures too.
    ///
    /// A failure needs a row to land on. The very first index of a project is
    /// also the one most likely to fail — a path that no longer exists, a
    /// service that is not running — and that project has never been on the
    /// published shelf, so a projection that skipped failures had nothing to
    /// mark and the refusal disappeared entirely.
    fn projected(&self) -> Vec<String> {
        self.jobs
            .iter()
            .filter(|job| job.kind == JobKind::Index)
            .map(|job| job.coordinate.clone())
            .collect()
    }

    /// Returns the job currently worth showing in the status bar.
    pub(crate) fn foreground(&self) -> Option<&Job> {
        self.jobs
            .iter()
            .find(|job| matches!(job.state, JobState::Failed(_)))
            .or_else(|| self.jobs.first())
    }

    /// Merges requested and failed projects into a projected shelf.
    ///
    /// The published shelf says what the engine committed. A window also knows
    /// what its reader just asked for, and a project whose index request was
    /// accepted a second ago belongs on the shelf marked `requested` rather
    /// than being invisible until the first rows land.
    ///
    /// A refused request gets that same row, marked failed. The row is
    /// projected before the failure is applied precisely so a project the
    /// engine has never heard of still has somewhere to say why.
    pub(crate) fn merge(&self, published: &Shelf) -> Shelf {
        let mut merged = project::with_requested(published, &self.projected());
        for job in &self.jobs {
            if let JobState::Failed(fault) = &job.state {
                merged = project::with_failure(&merged, &job.coordinate, fault.as_ref());
            }
        }
        merged
    }

    /// Submits one intent and tracks it until the service answers.
    pub(crate) fn submit(&mut self, kind: JobKind, coordinate: String, cx: &mut Context<Self>) {
        self.jobs.retain(|job| job.coordinate != coordinate);
        self.jobs.push(Job {
            coordinate: coordinate.clone(),
            kind,
            state: JobState::Submitted,
        });
        cx.emit(JobsEvent::Changed);
        cx.notify();
        let endpoint = self.endpoint.clone();
        let request = match kind {
            JobKind::Index => Request::Index {
                coordinate: coordinate.clone(),
            },
            JobKind::Remove => Request::Remove {
                coordinate: coordinate.clone(),
            },
        };
        let task = cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_spawn(async move { super::service::run(endpoint.path(), &request) })
                .await;
            let _ = this.update(cx, |this, cx| this.resolve(&coordinate, outcome, cx));
        });
        self.running.push(task);
    }

    /// Drops jobs whose project the shelf now reports as ready.
    pub(crate) fn reconcile(&mut self, published: &Shelf, cx: &mut Context<Self>) {
        let before = self.jobs.len();
        self.jobs.retain(|job| match job.kind {
            JobKind::Index => !project::find(published, &job.coordinate).is_some_and(|entry| {
                matches!(entry.readiness(), Readiness::Ready) && entry.declarations().get() > 0
            }),
            JobKind::Remove => project::find(published, &job.coordinate).is_some(),
        });
        if self.jobs.len() != before {
            cx.emit(JobsEvent::Changed);
            cx.notify();
        }
    }

    fn resolve(
        &mut self,
        coordinate: &str,
        outcome: Result<Outcome, backend_client::ClientError>,
        cx: &mut Context<Self>,
    ) {
        let Some(job) = self.jobs.iter_mut().find(|job| job.coordinate == coordinate) else {
            return;
        };
        job.state = match outcome {
            Ok(Outcome::Accepted) => JobState::Accepted,
            Ok(_) => JobState::Failed(Box::new(wrong_shape(
                operand_for(coordinate),
                "an accepted intent",
            ))),
            Err(error) => JobState::Failed(Box::new(Fault::from_client_error(
                &error,
                operand_for(coordinate),
            ))),
        };
        cx.emit(JobsEvent::Changed);
        cx.notify();
    }
}

fn operand_for(coordinate: &str) -> Operand {
    Operand::Coordinate(Coordinate::new(coordinate))
}

#[cfg(test)]
impl Job {
    /// Builds one job in a chosen state, so projections can be proven pure.
    pub(crate) fn fixture(kind: JobKind, coordinate: &str, state: JobState) -> Self {
        Self {
            coordinate: coordinate.to_owned(),
            kind,
            state,
        }
    }
}

#[cfg(test)]
impl JobsStore {
    /// Builds a store already holding these jobs, without touching a socket.
    pub(crate) fn fixture(jobs: Vec<Job>) -> Self {
        Self {
            endpoint: Endpoint::new("/tmp/jobs-fixture.sock"),
            jobs,
            running: Vec::new(),
        }
    }
}
