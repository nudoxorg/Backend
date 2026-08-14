//! The job registry behind the `index_package` tool.
//!
//! # Why an MCP tool needs a job registry at all
//!
//! Indexing a package is not a query. Fetching and lowering `serde` takes
//! seconds; `guava` takes considerably longer. An MCP call is a request/response
//! exchange over HTTP, so there are only three honest shapes available:
//!
//! 1. **Block until done.** Every client and proxy between the agent and this
//!    server gets to decide how long "until done" is allowed to be, and the
//!    agent's own turn stalls behind it.
//! 2. **Fire and forget.** The agent learns nothing, including whether the
//!    package name was even real — which is most of the value.
//! 3. **Bounded wait, resumable.** Wait up to a deadline the *caller* chose; if
//!    the job is still running when it expires, say so, say which stage it is
//!    in, and let the agent call again. This is what is implemented.
//!
//! Shape 3 only works if calling again *joins* the running job instead of
//! starting a second one. That is this module's entire reason to exist: without
//! it, an agent that polls twice runs rust-analyzer twice over the same sources,
//! and the second run races the first into
//! [`VersionRegistry::record`](crate::versions). The registry is keyed on
//! the *canonical* PURL, so `pkg:cargo/serde@1.0.196` and
//! `pkg:CARGO/serde@1.0.196` join the same job.
//!
//! # Cancellation
//!
//! `StreamHandle`'s `Drop` cancels the engine-side work, so the handle is held
//! by the job for as long as the job is live and dropped when the entry is
//! evicted. An agent that starts an index and disconnects therefore leaves a job
//! running to completion — deliberate, since the fetched package is useful to
//! the *next* caller and the expensive half is already paid.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::packages::acquire::Error;
use crate::wire::{Gen, SymbolKey};
use crate::{EngineHandle, IndexEvent, IndexStage, Integrity, Purl, StreamHandle};

/// How long a terminal job stays in the registry.
///
/// Long enough that an agent polling a slow index still finds the answer it was
/// waiting for; short enough that a long-lived server does not accumulate one
/// entry per package it has ever been asked about. A job evicted after
/// completion is not lost information — the package is in the corpus and
/// `list_packages` reports it.
const TERMINAL_RETENTION: Duration = Duration::from_secs(15 * 60);

/// What a job has reported so far.
#[derive(Clone, Debug)]
pub(crate) enum JobProgress {
    /// Still working.
    Running {
        /// The stage the engine last reported.
        stage: IndexStage,
        /// Bytes downloaded so far, if the job has reached the download.
        received: u64,
        /// Total bytes, when the registry advertised a length.
        total: Option<u64>,
    },
    /// Finished successfully; the package is in the corpus.
    Indexed(Box<Indexed>),
    /// Finished unsuccessfully.
    Failed(Error),
}

/// A completed index.
#[derive(Clone, Debug)]
pub(crate) struct Indexed {
    pub(crate) purl: String,
    pub(crate) name: String,
    pub(crate) ecosystem: String,
    pub(crate) version: String,
    pub(crate) symbol_count: u64,
    pub(crate) root: Option<SymbolKey>,
    pub(crate) integrity: Integrity,
}

struct Job {
    progress: tokio::sync::watch::Receiver<JobProgress>,
    /// Held, not used: dropping it cancels the engine-side work. See the module
    /// docs on cancellation.
    _stream: StreamHandle,
    started: Instant,
    finished: Mutex<Option<Instant>>,
}

/// Every index job this server has started, keyed by canonical PURL.
#[derive(Default)]
pub(crate) struct IndexJobs {
    jobs: Mutex<HashMap<String, Arc<Job>>>,
}

impl std::fmt::Debug for IndexJobs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IndexJobs").finish_non_exhaustive()
    }
}

impl IndexJobs {
    /// Start (or join) the job for `purl` and wait up to `deadline` for it to
    /// finish.
    ///
    /// Returns whatever the job's state is when the deadline expires — a
    /// `Running` return is not a failure and not a timeout error, it is the
    /// truthful answer to "has it finished yet".
    ///
    /// `joined` in the return says whether this call started the work or
    /// attached to work already in flight, because an agent that gets
    /// `Running` twice in a row needs to know it is not accidentally starting a
    /// new job on every poll.
    pub(crate) async fn run(
        &self,
        engine: &EngineHandle,
        purl: Purl,
        generation: Gen,
        deadline: Duration,
    ) -> (JobProgress, bool) {
        let key = purl.render();
        let (job, joined) = self.start_or_join(engine, purl, generation, &key);

        let mut progress = job.progress.clone();
        let outcome = tokio::time::timeout(deadline, async {
            loop {
                {
                    let current = progress.borrow_and_update().clone();
                    if !matches!(current, JobProgress::Running { .. }) {
                        return current;
                    }
                }
                if progress.changed().await.is_err() {
                    // The sender is gone without a terminal state. Report the
                    // last thing we know rather than inventing a success or a
                    // failure — the same call `McpError::TruncatedStream` makes
                    // one layer up.
                    return progress.borrow().clone();
                }
            }
        })
        .await;

        let progress = match outcome {
            Ok(terminal) => {
                *job.finished
                    .lock()
                    .expect("job clock lock is never held across a panic") = Some(Instant::now());
                terminal
            }
            Err(_elapsed) => job.progress.borrow().clone(),
        };
        (progress, joined)
    }

    /// How long the job for `purl` has been running, if one is live.
    pub(crate) fn elapsed(&self, purl: &str) -> Option<Duration> {
        let jobs = self
            .jobs
            .lock()
            .expect("index job lock is never held across a panic");
        jobs.get(purl).map(|j| j.started.elapsed())
    }

    fn start_or_join(
        &self,
        engine: &EngineHandle,
        purl: Purl,
        generation: Gen,
        key: &str,
    ) -> (Arc<Job>, bool) {
        let mut jobs = self
            .jobs
            .lock()
            .expect("index job lock is never held across a panic");

        prune(&mut jobs);

        if let Some(existing) = jobs.get(key) {
            return (Arc::clone(existing), true);
        }

        let (stream, rx) = engine.index_purl(purl, generation);
        let (tx, progress) = tokio::sync::watch::channel(JobProgress::Running {
            stage: IndexStage::Resolving,
            received: 0,
            total: None,
        });

        // Drain on the engine's runtime, not on this call's task: the job has to
        // outlive the request that started it, which is the whole point of the
        // bounded-wait shape.
        engine.runtime_handle().spawn(async move {
            while let Ok(event) = rx.recv_async().await {
                let next = match event {
                    IndexEvent::Started { .. } => continue,
                    IndexEvent::Stage {
                        stage,
                        received,
                        total,
                        ..
                    } => JobProgress::Running {
                        stage,
                        received,
                        total,
                    },
                    IndexEvent::Indexed {
                        purl,
                        name,
                        ecosystem,
                        version,
                        symbol_count,
                        root,
                        integrity,
                        ..
                    } => JobProgress::Indexed(Box::new(Indexed {
                        purl: purl.to_string(),
                        name: name.to_string(),
                        ecosystem: ecosystem.to_string(),
                        version: version.to_string(),
                        symbol_count,
                        root,
                        integrity,
                    })),
                    IndexEvent::Failed { error, .. } => JobProgress::Failed(error),
                    // `IndexEvent` is `#[non_exhaustive]`; a variant added later
                    // is progress we do not yet render, never a terminal state
                    // we might mistake for one.
                    _ => continue,
                };
                let terminal = !matches!(next, JobProgress::Running { .. });
                // `send` fails only when every receiver is gone, which means
                // nobody is waiting — the job still runs to completion, because
                // the package is worth having for the next caller.
                let _ = tx.send(next);
                if terminal {
                    break;
                }
            }
        });

        let job = Arc::new(Job {
            progress,
            _stream: stream,
            started: Instant::now(),
            finished: Mutex::new(None),
        });
        jobs.insert(key.to_owned(), Arc::clone(&job));
        (job, false)
    }
}

fn prune(jobs: &mut HashMap<String, Arc<Job>>) {
    jobs.retain(|_, job| {
        let finished = job
            .finished
            .lock()
            .expect("job clock lock is never held across a panic");
        match *finished {
            Some(at) => at.elapsed() < TERMINAL_RETENTION,
            None => true,
        }
    });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Two calls for the same package must join one job. Without this, an agent
    /// that polls a slow index runs the producer once per poll.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_second_call_for_the_same_purl_joins_the_running_job() {
        let engine = crate::Engine::start(
            crate::EngineConfig::default(),
            crate::store::source::fixtures::FixtureSource::rich(),
        );
        let jobs = IndexJobs::default();
        // A package URL that resolves to nothing reachable: the job fails, but
        // it fails *once*, and the second caller observes the same job.
        let purl = Purl::parse("pkg:cargo/nudox-package-that-does-not-exist@0.0.1")
            .expect("valid purl");

        let (_first, joined_first) = jobs
            .run(&engine, purl.clone(), Gen(1), Duration::from_millis(1))
            .await;
        let (_second, joined_second) = jobs
            .run(&engine, purl, Gen(2), Duration::from_millis(1))
            .await;

        assert!(!joined_first, "the first call must start the job");
        assert!(joined_second, "the second call must join it, not start another");
        drop(engine);
    }

    /// A deadline that expires reports the job's *state*, not an error. "Still
    /// downloading" is a true answer; a timeout error would be a false one.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_expired_deadline_reports_progress_rather_than_failing() {
        let engine = crate::Engine::start(
            crate::EngineConfig::default(),
            crate::store::source::fixtures::FixtureSource::rich(),
        );
        let jobs = IndexJobs::default();
        let purl = Purl::parse("pkg:cargo/serde@1.0.196").expect("valid purl");

        let (progress, _) = jobs
            .run(&engine, purl, Gen(1), Duration::from_nanos(1))
            .await;
        assert!(
            matches!(progress, JobProgress::Running { .. }),
            "a 1ns deadline cannot have completed a fetch: {progress:?}"
        );
        drop(engine);
    }
}
