//! The read lane: a small pool of read sessions for page data.
//!
//! Page reads (documents, neighbourhoods, outlines, dossiers, searches) run
//! on `N` worker threads, each owning its own session, so one slow read never
//! blocks the others. Index and admin work stays on the engine actor's own
//! lane. The pool never mutates the owner.
//!
//! Scheduling rules:
//! - **Latest wins per key.** Submitting a job for a key that is already
//!   queued replaces the queued job and cancels it; a running job for the key
//!   is cancelled (its token is set) and its result is dropped on landing by
//!   the store's generation check.
//! - **Priority.** Normal jobs run before prefetch jobs; FIFO within one
//!   priority.
//! - **Affinity.** A continuation page runs on the worker whose session
//!   issued the continuation, because the producer certificate lives there.
//! - **Wake, don't poll.** Every finished job is pushed to the result queue
//!   and the UI is woken through one coalescing [`WakeSender`].
//!
//! Mapping from replies to read models happens on the worker, so the UI
//! thread only ever receives finished models.

use super::actor::{ActorStartError, CancellationToken};
use super::liveness::{report_if_dead, transport_break};
use super::page_mapping::{self, OutlineIndex, PackageInputs, SymbolInputs};
use super::wake::{WakeReceiver, WakeSender, wake_channel};
use crate::core::{ErrorValue, FaultCode, LocalProjectId};
use crate::model::local_package::LocalPackageLoader;
use crate::model::pages::{
    Gap, GapReason, Generation, PackageRef, PageKey, PageValue, ReadFailure, SearchContinuation,
    SearchQuery, SymbolRef,
};
use backend_client::{ClientError, Session};
use backend_library::{
    CommandFailure, CommandReply, HealthReport, PageContinuation, PageTerminal, ProductText,
    ReplyDto, Row, SurfaceCommand, SurfaceReply, ViewSnapshot, ViewStateRoot,
};
use backend_present::{Engine, Probe};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};

/// Largest outline page the owner admits.
const OUTLINE_PAGE: u16 = 200;
/// Upper bound on outline pages read for one package.
const OUTLINE_PAGES: usize = 200;
/// Maximum rows and estimated retained bytes in one outline, including its
/// lookup maps. Limits are checked against borrowed rows before cloning.
const OUTLINE_ROWS: usize = 8_000;
const OUTLINE_BYTES: usize = 12 * 1024 * 1024;
/// Explore page size for the Orbit catalog.
const EXPLORE_LIMIT: u16 = 64;
/// Maximum queued page reads across all workers. Running reads have their
/// own fixed worker slots; a burst of distinct hover targets cannot grow the
/// queue or its keyed page slots without bound.
const MAX_QUEUED_READS: usize = 64;

/// What one job reads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReadRequest {
    /// A declaration page.
    Symbol(SymbolRef),
    /// A declaration's source view.
    Source(SymbolRef),
    /// A package dossier.
    Package(PackageRef),
    /// The first page of a search.
    Search(SearchQuery),
    /// A further page of a search.
    SearchMore {
        /// The query.
        query: SearchQuery,
        /// Continuation from the previous page.
        continuation: SearchContinuation,
    },
    /// The Orbit model.
    Orbit,
    /// The owner health model.
    Health,
    /// A browsing page's resource (your tree).
    Browse(crate::model::browse::BrowseKey),
}

impl ReadRequest {
    /// Returns the request that fills `key` from scratch.
    #[must_use]
    pub fn for_key(key: &PageKey) -> Self {
        match key {
            PageKey::Symbol(symbol) => Self::Symbol(symbol.clone()),
            PageKey::Source(symbol) => Self::Source(symbol.clone()),
            PageKey::Package(package) => Self::Package(package.clone()),
            PageKey::Search(query) => Self::Search(query.clone()),
            PageKey::Orbit => Self::Orbit,
            PageKey::Health => Self::Health,
            PageKey::Browse(key) => Self::Browse(key.clone()),
        }
    }
}

/// Scheduling class of one job.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Priority {
    /// Hover-intent prefetch: runs only when no normal job waits.
    Prefetch,
    /// A view is waiting on it.
    Normal,
}

/// One read job.
#[derive(Clone, Debug)]
pub struct ReadJob {
    /// Resource the result lands in.
    pub key: PageKey,
    /// What to read.
    pub request: ReadRequest,
    /// Store generation the result must match to land.
    pub generation: Generation,
    /// Scheduling class.
    pub priority: Priority,
    /// Cancellation shared with the store.
    pub cancel: CancellationToken,
    /// Worker the job must run on, for continuation pages.
    pub affinity: Option<usize>,
}

/// One finished job.
#[derive(Clone, Debug)]
pub struct ReadOutcome {
    /// Resource the result lands in.
    pub key: PageKey,
    /// Generation the job was issued with.
    pub generation: Generation,
    /// Worker that ran it.
    pub worker: usize,
    /// Scheduling class it ran at.
    pub priority: Priority,
    /// False for a useful partial page; the same generation is still reading.
    pub complete: bool,
    /// The read model, or why there is none.
    pub result: Result<PageValue, ReadFailure>,
}

/// Performs reads for one worker. Production uses [`SessionReader`].
pub trait PageReader: Send + 'static {
    /// Reads and maps one request. Implementations should check `cancel`
    /// between round trips and return [`ReadFailure::Cancelled`] when set.
    ///
    /// # Errors
    /// Returns the typed failure of the page's primary read; secondary reads
    /// that fail become gaps inside the returned model instead.
    fn read(
        &mut self,
        request: &ReadRequest,
        context: &ReadContext<'_>,
    ) -> Result<PageValue, ReadFailure>;
}

/// Worker-side context for one read.
pub struct ReadContext<'a> {
    /// Worker index (for continuation affinity).
    pub worker: usize,
    /// Cancellation token of the running job.
    pub cancel: &'a CancellationToken,
    /// Package outlines shared by every worker.
    pub outlines: &'a OutlineCache,
    /// Publishes a useful intermediate page before slower probes complete.
    pub progress: Option<&'a dyn Fn(PageValue)>,
}

impl std::fmt::Debug for ReadContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadContext")
            .field("worker", &self.worker)
            .finish_non_exhaustive()
    }
}

impl ReadContext<'_> {
    /// Delivers a partial page only while this read still owns its request.
    pub fn publish(&self, value: PageValue) {
        if !self.cancel.is_cancelled() {
            if let Some(progress) = self.progress {
                progress(value);
            }
        }
    }
}

/// One cached outline: package, revision root, index.
type OutlineEntry = (PackageRef, ViewStateRoot, Arc<OutlineIndex>, usize);

/// Package outlines shared across workers, keyed by package and revision.
#[derive(Debug, Default)]
pub struct OutlineCache {
    entries: Mutex<VecDeque<OutlineEntry>>,
}

impl OutlineCache {
    const CAPACITY: usize = 4;
    const MAX_BYTES: usize = 32 * 1024 * 1024;

    /// Returns a cached outline for `package` at `root`.
    #[must_use]
    pub fn get(&self, package: &PackageRef, root: ViewStateRoot) -> Option<Arc<OutlineIndex>> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let position = entries
            .iter()
            .position(|(cached, at, _, _)| cached == package && *at == root)?;
        let entry = entries.remove(position)?;
        let index = Arc::clone(&entry.2);
        entries.push_front(entry);
        Some(index)
    }

    /// Stores an outline, evicting the least recently used.
    pub fn put(
        &self,
        package: PackageRef,
        root: ViewStateRoot,
        index: Arc<OutlineIndex>,
        bytes: usize,
    ) {
        if !index.is_complete() || bytes > Self::MAX_BYTES {
            return;
        }
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.retain(|(cached, _, _, _)| *cached != package);
        entries.push_front((package, root, index, bytes));
        while entries.len() > Self::CAPACITY
            || entries.iter().map(|entry| entry.3).sum::<usize>() > Self::MAX_BYTES
        {
            entries.pop_back();
        }
    }
}

/// Conservative retained cost of one cloned row plus outline lookup entries.
/// String bytes are multiplied for the row, label/name maps, and allocator
/// slack; fixed overhead includes map nodes and child/name index vectors.
fn outline_row_bytes(row: &Row) -> usize {
    use backend_library::{Fragment, SourceExcerpt};
    let fragments = row.document.iter().fold(0_usize, |used, fragment| {
        let text = match fragment {
            Fragment::Text(text) | Fragment::Code(text) => text.len(),
            Fragment::Link { label, .. } => label.len(),
            Fragment::Break => 0,
        };
        used.saturating_add(std::mem::size_of::<Fragment>())
            .saturating_add(text)
    });
    let text = row
        .label
        .len()
        .saturating_add(
            row.identity_preimage()
                .map_or(0, |value| value.as_str().len()),
        )
        .saturating_add(row.signature.as_ref().map_or(0, String::len))
        .saturating_add(row.source.file_path().map_or(0, str::len))
        .saturating_add(match &row.excerpt {
            SourceExcerpt::Captured { text, .. } => text.len(),
            _ => 0,
        })
        .saturating_add(row.facts.text_bytes())
        .saturating_add(fragments);
    std::mem::size_of::<Row>()
        .saturating_add(512)
        .saturating_add(text.saturating_mul(4))
}

/// The job one worker is running: its key, generation, and token.
#[derive(Clone, Debug)]
struct RunningJob {
    key: PageKey,
    generation: Generation,
    cancel: CancellationToken,
}

/// What a worker is running, if anything.
type Running = Option<RunningJob>;

#[derive(Debug, Default)]
struct Queue {
    jobs: VecDeque<ReadJob>,
    running: Vec<Running>,
    closed: bool,
}

#[derive(Debug)]
struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
    results: Mutex<VecDeque<ReadOutcome>>,
    wake: WakeSender,
    outlines: OutlineCache,
}

impl Shared {
    fn queue(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A fixed pool of read workers.
pub struct ReadPool {
    shared: Arc<Shared>,
    workers: Vec<JoinHandle<()>>,
    wake: Option<WakeReceiver>,
}

impl std::fmt::Debug for ReadPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadPool")
            .field("workers", &self.workers.len())
            .field("queued", &self.queued())
            .finish_non_exhaustive()
    }
}

impl ReadPool {
    /// Starts `workers` threads, each with its own reader from `make`.
    ///
    /// # Errors
    /// Returns [`ActorStartError`] when a worker thread cannot start.
    pub fn start<R: PageReader>(
        workers: usize,
        make: impl Fn(usize) -> R,
    ) -> Result<Self, ActorStartError> {
        let (wake, receiver) = wake_channel();
        let workers = workers.max(1);
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue {
                running: vec![None; workers],
                ..Queue::default()
            }),
            ready: Condvar::new(),
            results: Mutex::new(VecDeque::new()),
            wake,
            outlines: OutlineCache::default(),
        });
        let mut pool = Self {
            shared,
            workers: Vec::with_capacity(workers),
            wake: Some(receiver),
        };
        for index in 0..workers {
            let reader = make(index);
            let shared = Arc::clone(&pool.shared);
            let handle = thread::Builder::new()
                .name(format!("nudox-read-{index}"))
                .spawn(move || run_worker(index, reader, &shared))
                .map_err(|error| ActorStartError::from_spawn_error(&error))?;
            pool.workers.push(handle);
        }
        Ok(pool)
    }

    /// Takes the wake receiver; the store's UI task awaits it.
    pub fn take_wake(&mut self) -> Option<WakeReceiver> {
        self.wake.take()
    }

    /// Returns the number of workers.
    #[must_use]
    pub fn workers(&self) -> usize {
        self.workers.len()
    }

    /// Queues one job. A queued job for the same key is replaced and
    /// cancelled; a running one is cancelled.
    pub fn submit(&self, job: ReadJob) -> bool {
        let mut queue = self.shared.queue();
        if queue.closed {
            return false;
        }
        queue.jobs.retain(|queued| {
            if queued.key == job.key {
                queued.cancel.cancel();
                false
            } else {
                true
            }
        });
        for running in queue.running.iter().flatten() {
            if running.key == job.key && running.generation != job.generation {
                running.cancel.cancel();
            }
        }
        if queue.jobs.len() >= MAX_QUEUED_READS {
            let victim = (job.priority == Priority::Normal)
                .then(|| {
                    queue
                        .jobs
                        .iter()
                        .position(|queued| queued.priority == Priority::Prefetch)
                })
                .flatten();
            if let Some(victim) = victim.and_then(|at| queue.jobs.remove(at)) {
                victim.cancel.cancel();
                self.shared
                    .results
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push_back(ReadOutcome {
                        key: victim.key,
                        generation: victim.generation,
                        worker: usize::MAX,
                        priority: victim.priority,
                        complete: true,
                        result: Err(ReadFailure::Cancelled),
                    });
                self.shared.wake.wake();
            } else {
                return false;
            }
        }
        queue.jobs.push_back(job);
        drop(queue);
        self.shared.ready.notify_all();
        true
    }

    /// Raises a queued job for `key` to normal priority. Returns whether a
    /// queued job was found (a running job needs no promotion).
    #[must_use]
    pub fn promote(&self, key: &PageKey) -> bool {
        let mut queue = self.shared.queue();
        let mut found = false;
        for job in queue.jobs.iter_mut().filter(|job| job.key == *key) {
            job.priority = Priority::Normal;
            found = true;
        }
        found
    }

    /// Cancels every queued or running job for `key`. Returns whether any job
    /// was found.
    #[must_use]
    pub fn cancel(&self, key: &PageKey) -> bool {
        let mut queue = self.shared.queue();
        let before = queue.jobs.len();
        queue.jobs.retain(|queued| {
            if queued.key == *key {
                queued.cancel.cancel();
                false
            } else {
                true
            }
        });
        let mut found = queue.jobs.len() != before;
        for running in queue.running.iter().flatten() {
            if running.key == *key {
                running.cancel.cancel();
                found = true;
            }
        }
        found
    }

    /// Drains every finished job without waiting.
    pub fn drain(&self) -> Vec<ReadOutcome> {
        let mut results = self
            .shared
            .results
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        results.drain(..).collect()
    }

    /// Returns the number of queued (not yet running) jobs.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.shared.queue().jobs.len()
    }

    /// Returns the number of jobs currently running.
    #[must_use]
    pub fn running(&self) -> usize {
        self.shared.queue().running.iter().flatten().count()
    }

    fn close_and_join(&mut self) {
        {
            let mut queue = self.shared.queue();
            queue.closed = true;
            for job in queue.jobs.drain(..) {
                job.cancel.cancel();
            }
            for running in queue.running.iter().flatten() {
                running.cancel.cancel();
            }
        }
        self.shared.ready.notify_all();
        for handle in self.workers.drain(..) {
            let _ = handle.join();
        }
        self.shared.wake.close();
    }
}

impl Drop for ReadPool {
    fn drop(&mut self) {
        self.close_and_join();
    }
}

fn next_job(queue: &mut Queue, worker: usize) -> Option<ReadJob> {
    let mut best: Option<(usize, Priority)> = None;
    for (index, job) in queue.jobs.iter().enumerate() {
        if job.affinity.is_some_and(|pinned| pinned != worker) {
            continue;
        }
        if best.is_none_or(|(_, priority)| job.priority > priority) {
            best = Some((index, job.priority));
        }
    }
    let (index, _) = best?;
    queue.jobs.remove(index)
}

fn run_worker<R: PageReader>(worker: usize, mut reader: R, shared: &Shared) {
    loop {
        let job = {
            let mut queue = shared.queue();
            loop {
                if queue.closed {
                    return;
                }
                if let Some(job) = next_job(&mut queue, worker) {
                    if let Some(slot) = queue.running.get_mut(worker) {
                        *slot = Some(RunningJob {
                            key: job.key.clone(),
                            generation: job.generation,
                            cancel: job.cancel.clone(),
                        });
                    }
                    break job;
                }
                queue = shared
                    .ready
                    .wait(queue)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        let result = if job.cancel.is_cancelled() {
            Err(ReadFailure::Cancelled)
        } else {
            let publish = |value| {
                shared
                    .results
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push_back(ReadOutcome {
                        key: job.key.clone(),
                        generation: job.generation,
                        worker,
                        priority: job.priority,
                        complete: false,
                        result: Ok(value),
                    });
                shared.wake.wake();
            };
            let context = ReadContext {
                worker,
                cancel: &job.cancel,
                outlines: &shared.outlines,
                progress: Some(&publish),
            };
            // A panicking reader must not take the worker (and every later
            // read) down with it; it becomes one typed fault.
            let _reading = super::traffic::Reading::begin();
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                reader.read(&job.request, &context)
            }))
            .unwrap_or_else(|_| {
                Err(ReadFailure::Fault(ErrorValue::new(
                    FaultCode::Protocol,
                    "the read worker panicked while mapping a reply",
                )))
            })
        };
        let result = if job.cancel.is_cancelled() {
            Err(ReadFailure::Cancelled)
        } else {
            result
        };
        {
            let mut queue = shared.queue();
            if let Some(slot) = queue.running.get_mut(worker) {
                *slot = None;
            }
        }
        shared
            .results
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(ReadOutcome {
                key: job.key,
                generation: job.generation,
                worker,
                priority: job.priority,
                complete: true,
                result,
            });
        shared.wake.wake();
    }
}

// ---------------------------------------------------------------------------
// Production reader
// ---------------------------------------------------------------------------

/// A lazily connected, reconnecting local session that answers
/// [`backend_present::Engine`] probes. Read-only: it refuses to index,
/// remove, or run a mutating surface command.
pub struct SessionEngine {
    endpoint: PathBuf,
    session: Option<Session>,
    /// Attached-owner generation that admitted the connected session.
    session_epoch: Option<super::owner::Epoch>,
    /// The owner this engine waits for on the read worker before use (I1).
    gate: Option<super::owner::OwnerGate>,
    /// The read job currently using this worker-owned session.
    cancel: Option<CancellationToken>,
}

impl std::fmt::Debug for SessionEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionEngine")
            .field("endpoint", &self.endpoint)
            .field("connected", &self.session.is_some())
            .finish()
    }
}

impl SessionEngine {
    /// Creates an engine that connects on first use, on the worker thread.
    #[must_use]
    pub fn new(endpoint: impl AsRef<Path>) -> Self {
        Self {
            endpoint: endpoint.as_ref().to_path_buf(),
            session: None,
            session_epoch: None,
            gate: None,
            cancel: None,
        }
    }

    /// An engine that waits on this worker thread for the owner to answer
    /// before a read or reconnect (the window may open before it does).
    #[must_use]
    pub fn gated(endpoint: impl AsRef<Path>, gate: super::owner::OwnerGate) -> Self {
        Self {
            gate: Some(gate),
            ..Self::new(endpoint)
        }
    }

    fn wait_for_owner(
        &self,
        gate: &super::owner::OwnerGate,
    ) -> Result<(), super::owner::OwnerFault> {
        match &self.cancel {
            Some(cancel) => gate.wait_cancelled(cancel),
            None => gate.wait(),
        }
    }

    fn with_session<T>(
        &mut self,
        mut operation: impl FnMut(&mut Session) -> Result<T, ClientError>,
    ) -> Result<T, ClientError> {
        if self
            .cancel
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            return Err(ClientError::Io(
                "read was cancelled before connecting".to_owned(),
            ));
        }
        let mut ready_epoch = None;
        if let Some(gate) = &self.gate {
            self.wait_for_owner(gate).map_err(|message| {
                ClientError::Io(format!("the index could not start: {message}"))
            })?;
            ready_epoch = gate.attached_ready_epoch();
            if self.session_epoch != ready_epoch {
                self.session = None;
                self.session_epoch = None;
            }
        }
        for attempt in 0..2 {
            if self
                .cancel
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
            {
                return Err(ClientError::Io(
                    "read was cancelled before retrying".to_owned(),
                ));
            }
            if self.session.is_none() {
                if let Some(gate) = &self.gate {
                    self.wait_for_owner(gate).map_err(|message| {
                        ClientError::Io(format!("the index could not start: {message}"))
                    })?;
                    ready_epoch = gate.attached_ready_epoch();
                }
                match Session::connect(&self.endpoint) {
                    Ok(session) => {
                        self.session = Some(session);
                        self.session_epoch = ready_epoch;
                    }
                    Err(error) if attempt == 0 && transport_break(&error) => continue,
                    Err(error) => {
                        if !self
                            .cancel
                            .as_ref()
                            .is_some_and(CancellationToken::is_cancelled)
                        {
                            let _ = report_if_dead(
                                self.gate.as_ref(),
                                ready_epoch,
                                &self.endpoint,
                                &error,
                            );
                        }
                        return Err(error);
                    }
                }
            }
            let Some(session) = self.session.as_mut() else {
                continue;
            };
            match operation(session) {
                Err(error) if attempt == 0 && transport_break(&error) => {
                    // Reads are idempotent; one reconnect-and-retry is safe.
                    self.session = None;
                    self.session_epoch = None;
                }
                Err(error) => {
                    if transport_break(&error) {
                        self.session = None;
                        self.session_epoch = None;
                        if !self
                            .cancel
                            .as_ref()
                            .is_some_and(CancellationToken::is_cancelled)
                        {
                            let _ = report_if_dead(
                                self.gate.as_ref(),
                                ready_epoch,
                                &self.endpoint,
                                &error,
                            );
                        }
                    }
                    return Err(error);
                }
                Ok(value) => return Ok(value),
            }
        }
        Err(ClientError::Protocol(
            "the local session could not be re-established".to_owned(),
        ))
    }
}

/// The page composer owns this narrow job lifecycle. Fixture engines may
/// ignore it; the production session binds it to every owner wait.
pub trait ReadEngine: Engine + Send + 'static {
    /// Binds the cancellation state of one page read, or clears it afterward.
    fn set_cancellation(&mut self, _cancel: Option<CancellationToken>) {}
}

impl ReadEngine for SessionEngine {
    fn set_cancellation(&mut self, cancel: Option<CancellationToken>) {
        self.cancel = cancel;
    }
}

pub(crate) const fn read_only(command: &SurfaceCommand) -> bool {
    matches!(
        command,
        SurfaceCommand::Advisory { .. }
            | SurfaceCommand::Read { .. }
            | SurfaceCommand::References { .. }
            | SurfaceCommand::Diff { .. }
            | SurfaceCommand::Explore { .. }
            | SurfaceCommand::Package { .. }
            | SurfaceCommand::ForgeReference { .. }
            | SurfaceCommand::Dependents { .. }
            | SurfaceCommand::Dependencies { .. }
            | SurfaceCommand::Owner { .. }
            | SurfaceCommand::IndexSearch { .. }
            | SurfaceCommand::PackageVersions { .. }
            | SurfaceCommand::SemanticVersions { .. }
            | SurfaceCommand::PackageProfile { .. }
            | SurfaceCommand::Subscriptions
            | SurfaceCommand::Projects
            | SurfaceCommand::Tree
            | SurfaceCommand::ProjectTree { .. }
    )
}

impl Engine for SessionEngine {
    fn revision(&mut self) -> Result<ViewStateRoot, ClientError> {
        self.with_session(|session| session.revision().map(|revision| revision.root))
    }

    fn health(&mut self) -> Result<HealthReport, ClientError> {
        self.with_session(Session::health)
    }

    fn probe(&mut self, probe: Probe<'_>) -> Result<ReplyDto, ClientError> {
        self.probe_page(probe, None)
    }

    fn probe_page(
        &mut self,
        probe: Probe<'_>,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        let asking = std::time::Instant::now();
        let name = match &probe {
            Probe::Packages => "probe.packages",
            Probe::Document(_) => "probe.document",
            Probe::Source(_) => "probe.source",
            Probe::Related(_) => "probe.related",
            Probe::Graph(_) => "probe.graph",
            Probe::Search { .. } => "probe.search",
            Probe::Names { .. } => "probe.names",
            Probe::Outline(_) | Probe::OutlinePage { .. } => "probe.outline",
            Probe::Index(_) | Probe::IndexWithExecutionIntent { .. } | Probe::Remove(_) => {
                "probe.mutation"
            }
        };
        let reply = self.with_session(|session| match probe {
            Probe::Packages => session.packages(),
            Probe::Document(at) => session.document(at),
            Probe::Source(at) => session.source(at),
            Probe::Related(at) => session.related(at),
            Probe::Graph(at) => session.graph(at),
            Probe::Search { text, limit } => session.search_page(text, limit, continuation),
            Probe::Names { text, limit } => session.names_page(text, limit, continuation),
            Probe::Outline(path) => session.outline(path),
            Probe::OutlinePage { path, limit } => session.outline_page(path, limit, continuation),
            Probe::Index(_) | Probe::IndexWithExecutionIntent { .. } | Probe::Remove(_) => Err(
                ClientError::Protocol("the read lane never mutates the owner".to_owned()),
            ),
        });
        super::trace::span(name, asking, "owner round trip");
        reply
    }

    fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
        if !read_only(&command) {
            return Err(ClientError::Protocol(
                "the read lane never runs a mutating surface command".to_owned(),
            ));
        }
        let asking = std::time::Instant::now();
        let name = super::trace::enabled().then(|| format!("{command:?}"));
        let reply = self.with_session(|session| session.surface(command.clone()));
        if let Some(name) = name {
            let variant = name
                .split(|c: char| !c.is_alphanumeric())
                .next()
                .unwrap_or("surface");
            super::trace::span("surface", asking, variant);
        }
        reply
    }
}

/// The production reader: composes each page from engine probes and maps
/// the replies on this worker.
pub struct SessionReader<E: Engine + Send + 'static = SessionEngine> {
    engine: E,
    loader: LocalPackageLoader,
}

impl<E: Engine + Send + 'static> std::fmt::Debug for SessionReader<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionReader").finish_non_exhaustive()
    }
}

impl SessionReader<SessionEngine> {
    /// Creates a reader for one local endpoint.
    #[must_use]
    pub fn connect(endpoint: impl AsRef<Path>) -> Self {
        Self::new(SessionEngine::new(endpoint), LocalPackageLoader::default())
    }

    /// A reader whose first connect waits for the owner, on its worker.
    #[must_use]
    pub fn gated(endpoint: impl AsRef<Path>, gate: super::owner::OwnerGate) -> Self {
        Self::new(
            SessionEngine::gated(endpoint, gate),
            LocalPackageLoader::default(),
        )
    }
}

impl<E: Engine + Send + 'static> SessionReader<E> {
    /// Wraps any engine (a fake in tests).
    #[must_use]
    pub const fn new(engine: E, loader: LocalPackageLoader) -> Self {
        Self { engine, loader }
    }
}

impl<E: ReadEngine> PageReader for SessionReader<E> {
    fn read(
        &mut self,
        request: &ReadRequest,
        context: &ReadContext<'_>,
    ) -> Result<PageValue, ReadFailure> {
        self.engine.set_cancellation(Some(context.cancel.clone()));
        let result = (|| match request {
            ReadRequest::Symbol(symbol) => compose_symbol(&mut self.engine, symbol, context),
            ReadRequest::Source(symbol) => compose_source(&mut self.engine, symbol, context),
            ReadRequest::Package(package) => {
                compose_package(&mut self.engine, &self.loader, package, context)
            }
            ReadRequest::Search(query) => {
                compose_search(&mut self.engine, query, None, context).map(PageValue::Search)
            }
            ReadRequest::SearchMore {
                query,
                continuation,
            } => compose_search(&mut self.engine, query, Some(continuation.cursor), context)
                .map(PageValue::SearchMore),
            ReadRequest::Orbit => compose_orbit(&mut self.engine, context),
            ReadRequest::Health => self
                .engine
                .health()
                .map(|report| PageValue::Health(page_mapping::health_model(&report)))
                .map_err(|error| failure(&error)),
            ReadRequest::Browse(key) => match key {
                crate::model::browse::BrowseKey::Tree(_) => {
                    super::browse_reads::compose(&mut self.engine, key)
                }
                crate::model::browse::BrowseKey::FindHome => {
                    compose_find(&mut self.engine, None, context).map(|page| {
                        PageValue::Browse(crate::model::browse::BrowseValue::Find(Arc::new(page)))
                    })
                }
                crate::model::browse::BrowseKey::Find(query) => {
                    compose_find(&mut self.engine, Some(query), context).map(|page| {
                        PageValue::Browse(crate::model::browse::BrowseValue::Find(Arc::new(page)))
                    })
                }
                crate::model::browse::BrowseKey::Compare(selection) => {
                    let mut packages = Vec::with_capacity(selection.packages().len());
                    let mut apis = Vec::with_capacity(selection.packages().len());
                    for package in selection.packages() {
                        check(context.cancel)?;
                        let PageValue::Package(dossier) =
                            compose_package(&mut self.engine, &self.loader, package, context)?
                        else {
                            return Err(shape("compare package"));
                        };
                        let api = match outline(&mut self.engine, package, context) {
                            Ok(index) => {
                                crate::model::pages::Known::Known(index.comparison_api(package))
                            }
                            Err(gap) => crate::model::pages::Known::Unknown(gap),
                        };
                        check(context.cancel)?;
                        packages.push(dossier);
                        apis.push(api);
                    }
                    let prepared = Arc::new(super::browse_views::prepare_compare(&packages, &apis));
                    Ok(PageValue::Browse(
                        crate::model::browse::BrowseValue::Compare(Arc::new(
                            crate::model::browse::CompareModel {
                                packages: packages.into(),
                                apis: apis.into(),
                                prepared,
                            },
                        )),
                    ))
                }
            },
        })();
        self.engine.set_cancellation(None);
        result
    }
}

// ---------------------------------------------------------------------------
// Composition: which probes each page needs
// ---------------------------------------------------------------------------

/// Lowers a client failure of the page's primary read.
#[must_use]
pub fn failure(error: &ClientError) -> ReadFailure {
    let code = match error {
        ClientError::CommandFailed(CommandFailure::NotFound) => FaultCode::Missing,
        ClientError::CommandFailed(_) | ClientError::Protocol(_) | ClientError::IncoherentView => {
            FaultCode::Protocol
        }
        ClientError::Disconnected(_)
        | ClientError::Io(_)
        | ClientError::Transport(_)
        | ClientError::RemoteDeadlineExceeded => FaultCode::Transport,
        ClientError::BasisMismatch { .. }
        | ClientError::FreshnessMismatch
        | ClientError::RequestMismatch { .. }
        | ClientError::CursorMismatch
        | ClientError::StaleCursor
        | ClientError::StaleSelection
        | ClientError::StaleRemoteRoot { .. }
        | ClientError::StaleRemoteCapability
        | ClientError::RemoteCapabilityRevoked => FaultCode::Cancelled,
    };
    ReadFailure::Fault(ErrorValue::new(code, error.to_string()))
}

fn shape(what: &str) -> ReadFailure {
    ReadFailure::Fault(ErrorValue::new(
        FaultCode::Protocol,
        format!("the {what} reply changed shape"),
    ))
}

fn check(cancel: &CancellationToken) -> Result<(), ReadFailure> {
    if cancel.is_cancelled() {
        Err(ReadFailure::Cancelled)
    } else {
        Ok(())
    }
}

/// A local alternate-release route carries its original registry root in
/// the page key. Verify both manifests and both memberships on this worker
/// before any page fact can be staged or returned. A lexical sibling path is
/// only a candidate; it never becomes authority by itself.
fn verify_release_origin(origin: Option<&str>, viewed: &PackageRef) -> Result<(), ReadFailure> {
    let Some(origin) = origin else {
        // A directly opened registry tree still needs its own manifest proof.
        // Its lexical cache stem is only a candidate for the page read.
        if viewed.registry_shape().is_some() && viewed.verify_registry_manifest().is_none() {
            return Err(ReadFailure::Fault(ErrorValue::new(
                FaultCode::Missing,
                "This Cargo source tree no longer matches its registry manifest",
            )));
        }
        return Ok(());
    };
    let refused = || {
        ReadFailure::Fault(ErrorValue::new(
            FaultCode::Missing,
            "This release could not be verified against its pinned Cargo source and local registry authority",
        ))
    };
    let pinned = PackageRef::parse(origin).map_err(|_| refused())?;
    let pinned_release = pinned.verify_registry_manifest().ok_or_else(refused)?;
    let viewed_release = viewed.verify_registry_manifest().ok_or_else(refused)?;
    if pinned_release.name != viewed_release.name {
        return Err(refused());
    }
    let composition = crate::host::registry::composed().ok_or_else(refused)?;
    if composition.source.release_of(Path::new(pinned.as_str())) != Some(pinned_release)
        || composition.source.release_of(Path::new(viewed.as_str())) != Some(viewed_release)
    {
        return Err(refused());
    }
    Ok(())
}

fn document(
    engine: &mut dyn Engine,
    probe: Probe<'_>,
) -> Result<backend_library::Document, ReadFailure> {
    match engine.probe(probe).map_err(|error| failure(&error))?.reply {
        CommandReply::Document(document) | CommandReply::Page(document) => Ok(document),
        _ => Err(shape("document")),
    }
}

fn related(engine: &mut dyn Engine, symbol: &SymbolRef) -> Result<ViewSnapshot, Gap> {
    match engine.probe(Probe::Related(symbol.as_str())) {
        Ok(reply) => match reply.reply {
            CommandReply::Graph(snapshot) => Ok(snapshot),
            _ => Err(Gap::new(
                GapReason::ReadFailed,
                "the related reply changed shape",
            )),
        },
        Err(error) => Err(page_mapping::client_gap(&error)),
    }
}

fn references(engine: &mut dyn Engine, symbol: &SymbolRef) -> Result<SurfaceReply, ClientError> {
    let target = ProductText::new(symbol.as_str())
        .map_err(|error| ClientError::Protocol(error.to_string()))?;
    engine.surface(SurfaceCommand::References { target })
}

/// Reads every outline page of one package (bounded), through the cache.
fn outline(
    engine: &mut dyn Engine,
    package: &PackageRef,
    context: &ReadContext<'_>,
) -> Result<Arc<OutlineIndex>, Gap> {
    let root = engine
        .revision()
        .map_err(|error| page_mapping::client_gap(&error))?;
    if let Some(cached) = context.outlines.get(package, root) {
        return Ok(cached);
    }
    let mut rows: Vec<Row> = Vec::new();
    let mut continuation = None;
    let mut complete = false;
    let mut retained_bytes = 0_usize;
    for _ in 0..OUTLINE_PAGES {
        if context.cancel.is_cancelled() {
            return Err(Gap::new(GapReason::ReadFailed, "cancelled"));
        }
        let reply = engine
            .probe_page(
                Probe::OutlinePage {
                    path: package.as_str(),
                    limit: OUTLINE_PAGE,
                },
                continuation,
            )
            .map_err(|error| page_mapping::client_gap(&error))?;
        let CommandReply::ProjectionPage(page) = reply.reply else {
            return Err(Gap::new(
                GapReason::ReadFailed,
                "the outline page reply changed shape",
            ));
        };
        let mut capped = false;
        for row in page.snapshot.root.rows() {
            let bytes = outline_row_bytes(row);
            if rows.len() >= OUTLINE_ROWS || retained_bytes.saturating_add(bytes) > OUTLINE_BYTES {
                capped = true;
                break;
            }
            retained_bytes += bytes;
            rows.push(row.clone());
        }
        if capped {
            break;
        }
        match page.terminal {
            PageTerminal::Complete => {
                complete = true;
                break;
            }
            PageTerminal::More(next) => continuation = Some(next),
            PageTerminal::Cancelled => break,
        }
    }
    let index = Arc::new(OutlineIndex::new(rows, complete));
    if complete {
        context
            .outlines
            .put(package.clone(), root, Arc::clone(&index), retained_bytes);
    }
    Ok(index)
}

/// The neighbourhood a symbol page reads: the declaration's own related
/// reply, plus — for a nominal type under a compiler publication — the
/// neighbourhoods of the impl blocks that reference it, because the engine
/// states "impl Trait for Type" only as type references from the impl block
/// and the trait sits one hop away from the type.
struct Hood {
    rows: Vec<Row>,
    relations: Option<Vec<backend_library::GraphRelation>>,
    rich: Option<backend_library::RichGraphSnapshot>,
}

fn neighbourhood(
    engine: &mut dyn Engine,
    symbol: &SymbolRef,
    context: &ReadContext<'_>,
) -> Result<Hood, Gap> {
    let snapshot = related(engine, symbol)?;
    let mut hood = Hood {
        rows: snapshot.root.rows().to_vec(),
        relations: snapshot
            .graph_relations
            .as_ref()
            .map(|edges| edges.to_vec()),
        rich: snapshot.rich_graph.clone(),
    };
    let Some(centre) = hood
        .rows
        .iter()
        .find(|row| row.label == symbol.as_str())
        .cloned()
    else {
        return Ok(hood);
    };
    let nominal = matches!(
        centre.kind,
        Some(
            backend_library::DeclarationKind::Struct
                | backend_library::DeclarationKind::Enum
                | backend_library::DeclarationKind::Union
                | backend_library::DeclarationKind::Class
        )
    );
    let Some(edges) = hood.relations.clone().filter(|_| nominal) else {
        return Ok(hood);
    };
    let blocks = hood
        .rows
        .iter()
        .filter(|row| {
            row.kind == Some(backend_library::DeclarationKind::Type)
                && row
                    .signature
                    .as_deref()
                    .is_some_and(|text| text.starts_with("nominal("))
                && edges.iter().any(|edge| {
                    edge.from == row.id
                        && edge.to == centre.id
                        && edge.relation == backend_library::SemanticLinkKind::TypeReference
                })
        })
        .filter_map(|row| SymbolRef::new(&row.label).ok())
        .take(16)
        .collect::<Vec<_>>();
    for block in blocks {
        if context.cancel.is_cancelled() {
            break;
        }
        let Ok(extra) = related(engine, &block) else {
            continue;
        };
        for row in extra.root.rows() {
            if !hood.rows.iter().any(|existing| existing.id == row.id) {
                hood.rows.push(row.clone());
            }
        }
        if let (Some(relations), Some(more)) =
            (hood.relations.as_mut(), extra.graph_relations.as_ref())
        {
            for edge in more {
                if !relations.contains(edge) {
                    relations.push(*edge);
                }
            }
        }
    }
    Ok(hood)
}

fn compose_symbol(
    engine: &mut dyn Engine,
    symbol: &SymbolRef,
    context: &ReadContext<'_>,
) -> Result<PageValue, ReadFailure> {
    let package = symbol.package().ok_or_else(|| shape("symbol package"))?;
    verify_release_origin(symbol.release_origin(), &package)?;
    let document = document(engine, Probe::Document(symbol.as_str()))?;
    check(context.cancel)?;
    let related = neighbourhood(engine, symbol, context);
    check(context.cancel)?;
    let references = references(engine, symbol);
    check(context.cancel)?;
    let reading_outline = Err(Gap::new(GapReason::Stale, "Reading the package outline"));
    context.publish(PageValue::Symbol(page_mapping::symbol_page(
        &SymbolInputs {
            coordinate: symbol,
            document: &document,
            related: related
                .as_ref()
                .map(|hood| page_mapping::Neighbourhood {
                    rows: &hood.rows,
                    relations: hood.relations.as_deref(),
                    rich: hood.rich.as_ref(),
                })
                .map_err(Clone::clone),
            references: references.as_ref(),
            outline: reading_outline,
        },
    )));
    let outline = symbol.package().map_or_else(
        || {
            Err(Gap::new(
                GapReason::NotCaptured,
                "the coordinate does not spell its package",
            ))
        },
        |package| outline(engine, &package, context),
    );
    check(context.cancel)?;
    let mut page = page_mapping::symbol_page(&SymbolInputs {
        coordinate: symbol,
        document: &document,
        related: related
            .as_ref()
            .map(|hood| page_mapping::Neighbourhood {
                rows: &hood.rows,
                relations: hood.relations.as_deref(),
                rich: hood.rich.as_ref(),
            })
            .map_err(Clone::clone),
        references: references.as_ref(),
        outline: outline.as_deref().map_err(Clone::clone),
    });
    // Your own files at each use's span: read here, on the worker, so the page
    // lands with its lines and nothing reads them again on the UI thread.
    if let Some(sites) = page.references.known() {
        page.workspace = super::workspace_lines::read(sites, &super::workspace_lines::OnDisk);
    }
    Ok(PageValue::Symbol(page))
}

/// Reads a local project file for the source view. Only a local package's
/// own files are read, and only by package-relative path.
struct LocalSourceFile {
    text: String,
    editor_path_hint: Option<String>,
}

const MAX_LOCAL_SOURCE_FILE_BYTES: u64 = 4 * 1024 * 1024;

fn local_file(package: &PackageRef, path: &str) -> Option<LocalSourceFile> {
    if !package.is_local() {
        return None;
    }
    let root = Path::new(package.as_str());
    if !root.is_absolute() {
        return None;
    }
    let (text, editor_path) = crate::model::local_package::files::read_text_under(
        root,
        Path::new(path),
        MAX_LOCAL_SOURCE_FILE_BYTES,
    )
    .ok()?;
    Some(LocalSourceFile {
        text,
        editor_path_hint: editor_path.and_then(|path| path.to_str().map(str::to_owned)),
    })
}

fn compose_source(
    engine: &mut dyn Engine,
    symbol: &SymbolRef,
    context: &ReadContext<'_>,
) -> Result<PageValue, ReadFailure> {
    let package = symbol.package().ok_or_else(|| shape("source package"))?;
    verify_release_origin(symbol.release_origin(), &package)?;
    let document = document(engine, Probe::Source(symbol.as_str()))?;
    check(context.cancel)?;
    let package = symbol.package();
    let outline = package
        .as_ref()
        .map(|package| outline(engine, package, context));
    check(context.cancel)?;
    let references_reply = references(engine, symbol);
    let outline_index = outline.as_ref().and_then(|result| result.as_ref().ok());
    let references =
        page_mapping::references(references_reply.as_ref(), outline_index.map(AsRef::as_ref));
    let local_file = match (&package, document.location.captured()) {
        (Some(package), Some(location)) => local_file(package, location.path()),
        _ => None,
    };
    Ok(PageValue::Source(page_mapping::source_view(
        symbol,
        &document,
        local_file.as_ref().map(|file| file.text.as_str()),
        local_file
            .as_ref()
            .and_then(|file| file.editor_path_hint.as_deref()),
        &references,
        outline_index.map(AsRef::as_ref),
    )))
}

fn compose_package(
    engine: &mut dyn Engine,
    loader: &LocalPackageLoader,
    package: &PackageRef,
    context: &ReadContext<'_>,
) -> Result<PageValue, ReadFailure> {
    verify_release_origin(package.release_origin(), package)?;
    // An alternate-release route is derived without filesystem work on the
    // UI lane. Verify the tree here, on the read worker, and report the
    // missing release instead of painting the pinned package's facts.
    if package.is_local() && !Path::new(package.as_str()).is_dir() {
        return Err(ReadFailure::Fault(ErrorValue::new(
            FaultCode::Missing,
            format!(
                "This package source is no longer on this machine: {}",
                package.as_str()
            ),
        )));
    }
    let reference = package.reference().clone();
    let records = engine.surface(SurfaceCommand::Package {
        package: reference.clone(),
    });
    check(context.cancel)?;
    let versions = engine.surface(SurfaceCommand::PackageVersions {
        package: reference.clone(),
    });
    check(context.cancel)?;
    let dependencies = engine.surface(SurfaceCommand::Dependencies {
        package: reference.clone(),
    });
    check(context.cancel)?;
    let dependents = engine.surface(SurfaceCommand::Dependents { package: reference });
    check(context.cancel)?;
    let engine_record = matches!(
        records,
        Ok(SurfaceReply::Package(ref rows)) if !rows.is_empty()
    );
    let local = package
        .is_local()
        .then(|| LocalProjectId::from_path(Path::new(package.as_str())).ok())
        .flatten()
        .filter(|project| project.path().is_dir())
        .and_then(|project| {
            if engine_record {
                loader.readme(&project)
            } else {
                Some(loader.load(&project))
            }
        });
    // Every part failing the same way means the package itself is unknown.
    if let (Err(error), None) = (&records, &local)
        && matches!(
            error,
            ClientError::Disconnected(_) | ClientError::Io(_) | ClientError::Transport(_)
        )
    {
        return Err(failure(error));
    }
    context.publish(PageValue::Package(page_mapping::package_dossier(
        &PackageInputs {
            package,
            records: records.as_ref(),
            versions: versions.as_ref(),
            dependencies: dependencies.as_ref(),
            dependents: dependents.as_ref(),
            outline: Err(Gap::new(GapReason::Stale, "Reading the package outline")),
            local: local.as_ref(),
        },
    )));
    let outline = outline(engine, package, context);
    check(context.cancel)?;
    Ok(PageValue::Package(page_mapping::package_dossier(
        &PackageInputs {
            package,
            records: records.as_ref(),
            versions: versions.as_ref(),
            dependencies: dependencies.as_ref(),
            dependents: dependents.as_ref(),
            outline: outline.as_deref().map_err(Clone::clone),
            local: local.as_ref(),
        },
    )))
}

fn compose_search(
    engine: &mut dyn Engine,
    query: &SearchQuery,
    continuation: Option<PageContinuation>,
    context: &ReadContext<'_>,
) -> Result<crate::model::pages::SearchPage, ReadFailure> {
    let reply = engine
        .probe_page(
            Probe::Search {
                text: &query.text,
                limit: query.limit,
            },
            continuation,
        )
        .map_err(|error| failure(&error))?;
    check(context.cancel)?;
    let semantic_search = reply.semantic_search_status();
    match reply.reply {
        CommandReply::Search(snapshot) => {
            let mut page = page_mapping::search_page(&query.text, &snapshot, context.worker);
            if let Some(status) = semantic_search {
                page.coverage = page.coverage.with_semantic_search_status(status);
            }
            Ok(page)
        }
        _ => Err(shape("search")),
    }
}

fn compose_find(
    engine: &mut dyn Engine,
    query: Option<&SearchQuery>,
    context: &ReadContext<'_>,
) -> Result<crate::model::browse::FindModel, ReadFailure> {
    use crate::model::browse::FindModel;
    use crate::model::pages::Known;
    let answers = match query {
        Some(query) => match compose_search(engine, query, None, context) {
            Ok(page) => Known::Known(page),
            Err(ReadFailure::Cancelled) => return Err(ReadFailure::Cancelled),
            Err(error) => Known::Unknown(Gap::new(GapReason::ReadFailed, format!("{error:?}"))),
        },
        None => Known::unknown(
            GapReason::NotCaptured,
            "Enter a name to find indexed declarations.",
        ),
    };
    check(context.cancel)?;
    let indexed = engine.probe(Probe::Packages);
    check(context.cancel)?;
    let query_text = query
        .map(|query| ProductText::new(query.text.to_string()))
        .transpose()
        .map_err(|_| shape("find query"))?;
    let catalog = engine.surface(SurfaceCommand::Explore {
        query: query_text,
        limit: EXPLORE_LIMIT,
    });
    check(context.cancel)?;
    let indexed_rows = match &indexed {
        Ok(reply) => match &reply.reply {
            CommandReply::Packages(rows) => Some(rows.root.rows().iter().collect::<Vec<_>>()),
            _ => None,
        },
        Err(_) => None,
    };
    let catalog_rows = match &catalog {
        Ok(SurfaceReply::Explored(records)) => Some(records.as_ref()),
        _ => None,
    };
    let registry = crate::host::registry::composed();
    let packages = super::browse_reads::find_packages(
        query.map_or("", |query| query.text.as_ref()),
        indexed_rows.as_deref().unwrap_or_default(),
        catalog_rows.unwrap_or_default(),
        registry.as_ref().map(|composed| composed.source.as_ref()),
    );
    let package_coverage = if indexed_rows.is_some() && catalog_rows.is_some() {
        Known::Known(())
    } else {
        Known::Unknown(Gap::new(
            GapReason::Unavailable,
            "Some package sources could not answer; these are the matches available locally.",
        ))
    };
    let prepared = Arc::new(super::browse_views::prepare_find(
        query.map_or("", |query| query.text.as_ref()),
        &answers,
        &packages,
        &package_coverage,
    ));
    Ok(FindModel {
        answers,
        packages: packages.into(),
        package_coverage,
        prepared,
    })
}

fn compose_orbit(
    engine: &mut dyn Engine,
    context: &ReadContext<'_>,
) -> Result<PageValue, ReadFailure> {
    let packages = engine
        .probe(Probe::Packages)
        .and_then(|reply| match reply.reply {
            CommandReply::Packages(snapshot) => Ok(snapshot),
            _ => Err(ClientError::Protocol(
                "the packages reply changed shape".to_owned(),
            )),
        });
    check(context.cancel)?;
    let projects = engine.surface(SurfaceCommand::Projects);
    check(context.cancel)?;
    let explore = engine.surface(SurfaceCommand::Explore {
        query: None,
        limit: EXPLORE_LIMIT,
    });
    check(context.cancel)?;
    let tree = engine.surface(SurfaceCommand::Tree);
    if let (Err(error), Err(_), Err(_), Err(_)) = (&packages, &projects, &explore, &tree) {
        return Err(failure(error));
    }
    Ok(PageValue::Orbit(page_mapping::orbit_model(
        &page_mapping::OrbitInputs {
            packages: packages.as_ref().map(|snapshot| snapshot.root.rows()),
            projects: projects.as_ref(),
            explore: explore.as_ref(),
            tree: tree.as_ref(),
        },
    )))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::model::ServiceMode;
    use crate::model::pages::{HealthModel, IngestModel};
    use crate::runtime::owner::{OwnerFault, OwnerGate, OwnerState};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    #[test]
    fn closing_the_read_pool_wakes_a_page_waiting_for_owner_startup() {
        let gate = super::super::owner::OwnerGate::starting();
        let worker_gate = gate.clone();
        let pool = ReadPool::start(1, move |_| {
            SessionReader::gated(
                "/tmp/nudox-no-owner-for-page-cancellation.sock",
                worker_gate.clone(),
            )
        })
        .expect("read pool");
        assert!(pool.submit(ReadJob {
            key: PageKey::Health,
            request: ReadRequest::Health,
            generation: Generation::new(1),
            priority: Priority::Normal,
            cancel: CancellationToken::new(),
            affinity: None,
        }));
        crate::runtime::wait::until("page entered the owner wait", || pool.running() == 1);
        let (sent, received) = mpsc::channel();
        std::thread::spawn(move || sent.send(drop(pool)).expect("read pool closed"));
        received
            .recv_timeout(Duration::from_secs(1))
            .expect("pool shutdown did not wait for the owner's 60-second patience");
        assert_eq!(gate.state(), super::super::owner::OwnerState::Starting);
    }

    #[test]
    fn outline_cache_evicts_by_retained_bytes_and_never_caches_incomplete_indexes() {
        let cache = OutlineCache::default();
        let root = backend_library::view_state_root(&[("outline".to_owned(), "cache".to_owned())]);
        let first = PackageRef::parse("pkg:cargo/first@1.0.0").expect("first");
        let second = PackageRef::parse("pkg:cargo/second@1.0.0").expect("second");
        cache.put(
            first.clone(),
            root,
            Arc::new(OutlineIndex::new(Vec::new(), true)),
            20 * 1024 * 1024,
        );
        cache.put(
            second.clone(),
            root,
            Arc::new(OutlineIndex::new(Vec::new(), true)),
            20 * 1024 * 1024,
        );
        assert!(
            cache.get(&first, root).is_none(),
            "the combined retained cost exceeds the cache budget"
        );
        assert!(cache.get(&second, root).is_some());
        cache.put(
            first.clone(),
            root,
            Arc::new(OutlineIndex::new(Vec::new(), false)),
            1,
        );
        assert!(
            cache.get(&first, root).is_none(),
            "an incomplete outline cannot become a cache hit"
        );
    }

    /// A reader whose `Symbol` reads block until the test releases them, and
    /// that records the order jobs started in.
    struct GatedReader {
        gate: Arc<(Mutex<BTreeSetOf>, Condvar)>,
        started: mpsc::Sender<(usize, String)>,
    }

    type BTreeSetOf = std::collections::BTreeSet<String>;

    fn health() -> HealthModel {
        HealthModel {
            lanes: backend_present::CoverageLine::new(&[], None),
            rows: 0,
            ingest: IngestModel {
                files_discovered: 0,
                files_indexed: 0,
                files_unavailable: 0,
                declarations: 0,
                languages: Arc::from([]),
                faults: Arc::from([]),
            },
            ready_capabilities: Arc::from([]),
            missing_capabilities: Arc::from([]),
        }
    }

    #[test]
    fn alternate_release_refuses_a_sibling_when_the_original_manifest_disagrees() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let scratch = std::env::temp_dir().join(format!(
            "nudox-release-origin-{}-{nonce}",
            std::process::id()
        ));
        let source = scratch.join("registry/src/index.test-0");
        let original = source.join("fake-1.0.0");
        std::fs::create_dir_all(&original).expect("original tree");
        std::fs::write(
            original.join("Cargo.toml"),
            b"[package]\nname = \"other\"\nversion = \"1.0.0\"\n",
        )
        .expect("misnamed manifest");
        let viewed = PackageRef::parse(source.join("fake-2.0.0").to_str().expect("UTF-8"))
            .expect("candidate");
        assert!(matches!(
            verify_release_origin(Some(original.to_str().expect("UTF-8")), &viewed),
            Err(ReadFailure::Fault(error)) if error.code() == FaultCode::Missing
        ));
        std::fs::remove_dir_all(&scratch).expect("remove scratch");
    }

    #[test]
    fn worker_publishes_a_partial_page_before_its_slow_probe_finishes() {
        struct StagedReader {
            begun: mpsc::Sender<()>,
            release: Arc<std::sync::atomic::AtomicBool>,
        }
        impl PageReader for StagedReader {
            fn read(
                &mut self,
                _: &ReadRequest,
                context: &ReadContext<'_>,
            ) -> Result<PageValue, ReadFailure> {
                context.publish(PageValue::Health(health()));
                self.begun.send(()).expect("stage signal");
                while !self.release.load(std::sync::atomic::Ordering::Acquire) {
                    if context.cancel.is_cancelled() {
                        return Err(ReadFailure::Cancelled);
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Ok(PageValue::Health(health()))
            }
        }
        let (begun, started) = mpsc::channel();
        let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let pool = ReadPool::start(1, |_| StagedReader {
            begun: begun.clone(),
            release: Arc::clone(&release),
        })
        .expect("read pool");
        pool.submit(ReadJob {
            key: PageKey::Health,
            request: ReadRequest::Health,
            generation: Generation::new(1),
            priority: Priority::Normal,
            cancel: CancellationToken::new(),
            affinity: None,
        });
        started
            .recv_timeout(Duration::from_secs(2))
            .expect("stage reached UI queue");
        let staged = pool.drain();
        assert_eq!(staged.len(), 1);
        assert!(!staged[0].complete);
        assert_eq!(pool.running(), 1, "the worker continues its slow probe");
        release.store(true, std::sync::atomic::Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(2);
        let final_result = loop {
            let results = pool.drain();
            if let Some(result) = results.into_iter().find(|result| result.complete) {
                break result;
            }
            assert!(Instant::now() < deadline, "final read did not finish");
            std::thread::sleep(Duration::from_millis(1));
        };
        assert!(matches!(final_result.result, Ok(PageValue::Health(_))));
    }

    impl PageReader for GatedReader {
        fn read(
            &mut self,
            request: &ReadRequest,
            context: &ReadContext<'_>,
        ) -> Result<PageValue, ReadFailure> {
            let name = match request {
                ReadRequest::Symbol(symbol) => symbol.as_str().to_owned(),
                other => format!("{other:?}"),
            };
            let _ = self.started.send((context.worker, name.clone()));
            let (lock, released) = &*self.gate;
            let mut open = lock.lock().unwrap_or_else(PoisonError::into_inner);
            let deadline = Instant::now() + Duration::from_secs(10);
            while name.starts_with("slow") && !open.contains(&name) {
                if context.cancel.is_cancelled() {
                    return Err(ReadFailure::Cancelled);
                }
                assert!(Instant::now() < deadline, "gate never opened for {name}");
                open = released
                    .wait_timeout(open, Duration::from_millis(5))
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
            Ok(PageValue::Health(health()))
        }
    }

    struct Harness {
        pool: ReadPool,
        gate: Arc<(Mutex<BTreeSetOf>, Condvar)>,
        started: mpsc::Receiver<(usize, String)>,
    }

    fn harness(workers: usize) -> Harness {
        let gate = Arc::new((Mutex::new(BTreeSetOf::new()), Condvar::new()));
        let (sender, started) = mpsc::channel();
        let pool_gate = Arc::clone(&gate);
        let pool = ReadPool::start(workers, move |_| GatedReader {
            gate: Arc::clone(&pool_gate),
            started: sender.clone(),
        })
        .expect("pool");
        Harness {
            pool,
            gate,
            started,
        }
    }

    impl Harness {
        fn release(&self, name: &str) {
            let (lock, released) = &*self.gate;
            lock.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(name.to_owned());
            released.notify_all();
        }

        fn started(&self) -> (usize, String) {
            self.started
                .recv_timeout(Duration::from_secs(10))
                .expect("a job started")
        }

        fn outcomes(&self, count: usize) -> Vec<ReadOutcome> {
            let mut outcomes = Vec::new();
            crate::runtime::wait::until(format!("{count} outcomes arrived"), || {
                outcomes.extend(self.pool.drain());
                outcomes.len() >= count
            });
            outcomes
        }
    }

    fn job(name: &str, generation: u64, priority: Priority) -> ReadJob {
        let generation = Generation::new(generation);
        let symbol = SymbolRef::new(name).expect("symbol");
        ReadJob {
            key: PageKey::Symbol(symbol.clone()),
            request: ReadRequest::Symbol(symbol),
            generation,
            priority,
            cancel: CancellationToken::new(),
            affinity: None,
        }
    }

    fn key(name: &str) -> PageKey {
        PageKey::Symbol(SymbolRef::new(name).expect("symbol"))
    }

    #[test]
    fn a_slow_read_never_blocks_the_other_sessions() {
        let harness = harness(3);
        harness.pool.submit(job("slow-a", 1, Priority::Normal));
        assert_eq!(harness.started().1, "slow-a");
        harness.pool.submit(job("fast-b", 2, Priority::Normal));
        harness.pool.submit(job("fast-c", 3, Priority::Normal));
        let done = harness.outcomes(2);
        let mut names = done
            .iter()
            .map(|outcome| outcome.key.to_string())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, ["symbol fast-b", "symbol fast-c"]);
        assert_eq!(harness.pool.running(), 1, "the slow read is still running");
        harness.release("slow-a");
        let slow = harness.outcomes(1);
        assert_eq!(slow[0].key, key("slow-a"));
        assert!(slow[0].result.is_ok());
    }

    #[test]
    fn a_newer_request_for_the_same_key_cancels_the_older_one() {
        let harness = harness(1);
        let older = job("slow-k", 1, Priority::Normal);
        let older_token = older.cancel.clone();
        harness.pool.submit(older);
        assert_eq!(harness.started().1, "slow-k");
        // Queue then replace a second job for another key: only the newest runs.
        let queued = job("slow-q", 2, Priority::Normal);
        let queued_token = queued.cancel.clone();
        harness.pool.submit(queued);
        harness.pool.submit(job("slow-q", 3, Priority::Normal));
        assert!(
            queued_token.is_cancelled(),
            "the replaced queued job was cancelled"
        );
        assert_eq!(harness.pool.queued(), 1);
        // A newer generation for the running key cancels the running job.
        harness.pool.submit(job("slow-k", 4, Priority::Normal));
        assert!(
            older_token.is_cancelled(),
            "the running job was told to stop"
        );
        let first = harness.outcomes(1);
        assert_eq!(
            (first[0].key.clone(), first[0].generation),
            (key("slow-k"), Generation::new(1))
        );
        assert_eq!(first[0].result, Err(ReadFailure::Cancelled));
        harness.release("slow-q");
        harness.release("slow-k");
        let rest = harness.outcomes(2);
        let mut ran = rest
            .iter()
            .map(|outcome| (outcome.key.to_string(), outcome.generation))
            .collect::<Vec<_>>();
        ran.sort();
        assert_eq!(
            ran,
            [
                ("symbol slow-k".to_owned(), Generation::new(4)),
                ("symbol slow-q".to_owned(), Generation::new(3))
            ]
        );
    }

    #[test]
    fn normal_reads_run_before_prefetches_and_prefetches_cancel() {
        let harness = harness(1);
        harness.pool.submit(job("slow-busy", 1, Priority::Normal));
        assert_eq!(harness.started().1, "slow-busy");
        harness
            .pool
            .submit(job("fast-hover", 2, Priority::Prefetch));
        harness
            .pool
            .submit(job("fast-dropped", 3, Priority::Prefetch));
        harness.pool.submit(job("fast-click", 4, Priority::Normal));
        assert!(harness.pool.cancel(&key("fast-dropped")));
        assert_eq!(harness.pool.queued(), 2);
        harness.release("slow-busy");
        assert_eq!(harness.started().1, "fast-click");
        assert_eq!(harness.started().1, "fast-hover");
        let done = harness.outcomes(3);
        assert!(
            done.iter()
                .all(|outcome| outcome.key != key("fast-dropped"))
        );
    }

    #[test]
    fn a_promoted_prefetch_jumps_the_queue() {
        let harness = harness(1);
        harness.pool.submit(job("slow-busy", 1, Priority::Normal));
        assert_eq!(harness.started().1, "slow-busy");
        harness
            .pool
            .submit(job("fast-hover", 2, Priority::Prefetch));
        harness.pool.submit(job("fast-other", 3, Priority::Normal));
        assert!(harness.pool.promote(&key("fast-hover")));
        harness.release("slow-busy");
        // Both are normal now; FIFO puts the promoted prefetch first.
        assert_eq!(harness.started().1, "fast-hover");
        assert_eq!(harness.started().1, "fast-other");
    }

    #[test]
    fn a_continuation_runs_on_the_session_that_issued_it() {
        let harness = harness(3);
        for round in 0..6 {
            let mut pinned = job(&format!("fast-{round}"), round, Priority::Normal);
            pinned.affinity = Some(2);
            harness.pool.submit(pinned);
        }
        let done = harness.outcomes(6);
        assert!(done.iter().all(|outcome| outcome.worker == 2));
    }

    #[test]
    fn every_finished_read_wakes_the_ui_once_per_drain() {
        let mut harness = harness(2);
        let mut receiver = harness.pool.take_wake().expect("wake receiver");
        for round in 0..8 {
            harness
                .pool
                .submit(job(&format!("fast-{round}"), round, Priority::Normal));
        }
        let done = harness.outcomes(8);
        assert_eq!(done.len(), 8);
        let (signals, _) = receiver.counts();
        assert_eq!(signals, 8, "one wake per finished read");
        assert!(receiver.try_take(), "the burst left one pending turn");
        assert!(!receiver.try_take(), "and only one");
    }

    #[test]
    fn editor_paths_are_canonical_and_cannot_escape_the_local_package() {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "nudox-source-path-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("src")).expect("package source directory");
        std::fs::write(root.join("src/lib.rs"), "pub fn owned() {}\n").expect("source file");
        let package = PackageRef::parse(root.to_str().expect("UTF-8 temp path")).expect("package");
        let opened = local_file(&package, "src/lib.rs").expect("local source");
        assert_eq!(opened.text, "pub fn owned() {}\n");
        let expected = root
            .join("src/lib.rs")
            .canonicalize()
            .expect("canonical source");
        assert_eq!(
            std::path::Path::new(opened.editor_path_hint.as_deref().expect("editor hint")),
            expected.as_path()
        );
        assert!(local_file(&package, "../outside.rs").is_none());

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let outside = root.with_extension("outside.rs");
            std::fs::write(&outside, "pub fn outside() {}\n").expect("outside source");
            symlink(&outside, root.join("src/outside.rs")).expect("source symlink");
            assert!(local_file(&package, "src/outside.rs").is_none());
            std::fs::remove_file(outside).expect("remove outside fixture");
        }
        std::fs::remove_dir_all(root).expect("remove source fixture");
    }

    #[cfg(unix)]
    #[test]
    fn held_source_capability_rejects_replaced_links_and_special_files() {
        use std::io::Read as _;
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!("nudox-source-held-{}", std::process::id()));
        let _removed = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).expect("source directory");
        std::fs::write(root.join("src/lib.rs"), "inside").expect("source file");
        let capability =
            backend_platform::directory::DirectoryCapability::open_read_only_source(&root)
                .expect("pin package root");

        let mut held = crate::model::local_package::files::open_relative_source(
            &capability,
            Path::new("src/lib.rs"),
        )
        .expect("open source relative to held root");
        let mut content = String::new();
        held.read_to_string(&mut content)
            .expect("read opened source");
        assert_eq!(content, "inside");

        std::fs::remove_file(root.join("src/lib.rs")).expect("remove original path");
        let outside = root.with_extension("outside.rs");
        std::fs::write(&outside, "outside").expect("outside file");
        symlink(&outside, root.join("src/lib.rs")).expect("replace with symlink");
        assert!(
            crate::model::local_package::files::open_relative_source(
                &capability,
                Path::new("src/lib.rs"),
            )
            .is_err()
        );

        let fifo = root.join("src/fifo");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo is available on Unix");
        assert!(status.success(), "create FIFO fixture");
        assert!(
            crate::model::local_package::files::open_relative_source(
                &capability,
                Path::new("src/fifo"),
            )
            .is_err()
        );

        std::fs::remove_file(outside).expect("remove outside fixture");
        std::fs::remove_dir_all(root).expect("remove source fixture");
    }
}
