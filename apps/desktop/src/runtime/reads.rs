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
use crate::host::registry::{RegistryPackageIdentity, RegistrySource};
use crate::model::browse::{BrowseValue, CargoSourceInventoryKey, CargoSourceInventoryModel};
use crate::model::local_package::LocalPackageLoader;
use crate::model::pages::{
    CargoSourceKey, CargoSourcePage, Gap, GapReason, Generation, PackageRef, PageKey, PageValue,
    ReadFailure, SearchContinuation, SearchQuery, SymbolRef, VerifiedRegistryRelease,
};
use backend_client::{ClientError, Session};
use backend_library::{
    CargoPackageSourceFileResultV1, CargoPackageSourceInventoryFailureV1,
    CargoPackageSourceInventoryResultV1, CargoPackageSourcePathV1, CargoPackageSourceReadFailureV1,
    CargoPackageSourceRequestV1, CargoPackageSourceSemanticStatusV1, CommandFailure, CommandReply,
    HealthReport, PageContinuation, PageTerminal, ProductText, ReplyDto, Row, SurfaceCommand,
    SurfaceReply, ViewSnapshot, ViewStateRoot,
};
use backend_present::{Engine, Probe};
use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

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
/// Count bound covering queued, running, published and drained reads. This
/// bounds admitted read lifecycles, not payload copies, heap bytes or landed values.
const MAX_ADMITTED_READS: usize = 64;
/// Leave half the admission slots available to visible/normal requests.
const MAX_PREFETCH_ADMISSION: usize = MAX_ADMITTED_READS / 2;
/// Maximum landings in one UI update.
const LANDING_BUDGET: usize = 8;
/// Maximum local Cargo-shaped roots for which one Orbit read admits registry
/// identity. Above this bound it grants no partial proof, since an unseen row
/// could make the exact release ambiguous.
const MAX_REGISTRY_ROOTS_PER_ORBIT: usize = 2_048;

/// What one job reads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReadRequest {
    /// A declaration page.
    Symbol(SymbolRef),
    /// A declaration's source view.
    Source(SymbolRef),
    /// A package-relative file under exact Cargo source authority.
    CargoSource(CargoSourceKey),
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
            PageKey::CargoSource(file) => Self::CargoSource(file.clone()),
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
    // Clones and drained partials keep the same admission alive. This is
    // scheduling ownership only; key/generation still decide admission.
    _residency: Arc<ReadPermit>,
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

/// One admission, shared by its worker and every outstanding publication.
/// Releasing it never needs a scheduler lock, including during shutdown.
#[derive(Debug)]
struct ReadPermit(Arc<AtomicUsize>);

impl ReadPermit {
    fn acquire(count: &Arc<AtomicUsize>, priority: Priority) -> Option<Arc<Self>> {
        let limit = match priority {
            Priority::Normal => MAX_ADMITTED_READS,
            Priority::Prefetch => MAX_PREFETCH_ADMISSION,
        };
        count
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                (used < limit).then_some(used + 1)
            })
            .ok()?;
        Some(Arc::new(Self(Arc::clone(count))))
    }
}

impl Drop for ReadPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
struct AdmittedRead {
    job: ReadJob,
    permit: Arc<ReadPermit>,
}

impl std::ops::Deref for AdmittedRead {
    type Target = ReadJob;
    fn deref(&self) -> &ReadJob {
        &self.job
    }
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
    jobs: VecDeque<AdmittedRead>,
    running: Vec<Running>,
    closed: bool,
}

impl Queue {
    /// Detach replaced jobs while releasing their admissions immediately.
    /// Cancellation callbacks run only after the queue guard is released.
    fn remove_queued(&mut self, key: &PageKey) -> Vec<CancellationToken> {
        let mut tokens = Vec::new();
        self.jobs.retain(|queued| {
            if queued.key == *key {
                tokens.push(queued.cancel.clone());
                false
            } else {
                true
            }
        });
        tokens
    }
}

#[derive(Debug)]
struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
    results: Mutex<VecDeque<ReadOutcome>>,
    wake: WakeSender,
    outlines: OutlineCache,
    admitted: Arc<AtomicUsize>,
}

impl Shared {
    fn queue(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keep one queued partial per key/generation. A terminal result replaces
    /// that partial, but terminal results are never evicted by publications.
    fn publish(&self, outcome: ReadOutcome) {
        let mut results = self.results.lock().unwrap_or_else(PoisonError::into_inner);
        let same = |queued: &ReadOutcome| {
            queued.key == outcome.key && queued.generation == outcome.generation
        };
        if !outcome.complete && results.iter().any(|queued| same(queued) && queued.complete) {
            return;
        }
        let old = results
            .iter()
            .position(|queued| same(queued) && !queued.complete)
            .and_then(|at| results.remove(at));
        results.push_back(outcome);
        drop(results);
        // Large replaced models are destroyed outside the scheduler lock.
        drop(old);
        self.wake.wake();
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
            admitted: Arc::new(AtomicUsize::new(0)),
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
        let mut cancelled = queue.remove_queued(&job.key);
        cancelled.extend(
            queue
                .running
                .iter()
                .flatten()
                .filter(|running| running.key == job.key && running.generation != job.generation)
                .map(|running| running.cancel.clone()),
        );
        let accepted =
            if let Some(permit) = ReadPermit::acquire(&self.shared.admitted, job.priority) {
                queue.jobs.push_back(AdmittedRead { job, permit });
                true
            } else {
                false
            };
        drop(queue);
        for token in cancelled {
            token.cancel();
        }
        if accepted {
            self.shared.ready.notify_all();
        }
        accepted
    }

    /// Raises a queued job for `key` to normal priority. Returns whether a
    /// queued job was found (a running job needs no promotion).
    #[must_use]
    pub fn promote(&self, key: &PageKey) -> bool {
        let mut queue = self.shared.queue();
        let mut found = false;
        for job in queue.jobs.iter_mut().filter(|job| job.key == *key) {
            job.job.priority = Priority::Normal;
            found = true;
        }
        found
    }

    /// Cancels every queued or running job for `key`. Returns whether any job
    /// was found.
    #[must_use]
    pub fn cancel(&self, key: &PageKey) -> bool {
        let mut queue = self.shared.queue();
        let mut cancelled = queue.remove_queued(key);
        cancelled.extend(
            queue
                .running
                .iter()
                .flatten()
                .filter(|running| running.key == *key)
                .map(|running| running.cancel.clone()),
        );
        drop(queue);
        let found = !cancelled.is_empty();
        for token in cancelled {
            token.cancel();
        }
        found
    }

    /// Takes one bounded batch without waiting. Remaining results rearm the
    /// same coalesced wake; the UI consumer yields between batches. Normal
    /// results lead, with one slot in a full batch reserved for pending
    /// prefetch results so continuous normal work cannot starve them.
    pub fn drain(&self) -> Vec<ReadOutcome> {
        self.drain_for(&BTreeSet::new())
    }

    /// Visible keys also lead when their read started as a prefetch. Borrow
    /// the current selection rather than retaining it in worker state.
    pub(crate) fn drain_for(&self, visible: &BTreeSet<PageKey>) -> Vec<ReadOutcome> {
        let mut results = self
            .shared
            .results
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let count = results.len().min(LANDING_BUDGET);
        let mut batch = Vec::with_capacity(count);
        for slot in 0..count {
            let preferred = if slot + 1 == LANDING_BUDGET {
                Priority::Prefetch
            } else {
                Priority::Normal
            };
            let at = results
                .iter()
                .position(|outcome| {
                    let normal =
                        outcome.priority == Priority::Normal || visible.contains(&outcome.key);
                    normal == (preferred == Priority::Normal)
                })
                .unwrap_or(0);
            if let Some(outcome) = results.remove(at) {
                batch.push(outcome);
            }
        }
        let remaining = !results.is_empty();
        drop(results);
        if remaining {
            self.shared.wake.wake();
        }
        batch
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
        let cancelled = {
            let mut queue = self.shared.queue();
            queue.closed = true;
            let mut cancelled = queue
                .jobs
                .drain(..)
                .map(|job| job.job.cancel)
                .collect::<Vec<_>>();
            cancelled.extend(
                queue
                    .running
                    .iter()
                    .flatten()
                    .map(|running| running.cancel.clone()),
            );
            cancelled
        };
        for token in cancelled {
            token.cancel();
        }
        self.shared.ready.notify_all();
        for handle in self.workers.drain(..) {
            let _ = handle.join();
        }
        self.shared
            .results
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        self.shared.wake.close();
    }
}

impl Drop for ReadPool {
    fn drop(&mut self) {
        self.close_and_join();
    }
}

fn next_job(queue: &mut Queue, worker: usize) -> Option<AdmittedRead> {
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
        let AdmittedRead { job, permit } = job;
        let result = if job.cancel.is_cancelled() {
            Err(ReadFailure::Cancelled)
        } else {
            let publish = |value| {
                shared.publish(ReadOutcome {
                    key: job.key.clone(),
                    generation: job.generation,
                    worker,
                    priority: job.priority,
                    complete: false,
                    result: Ok(value),
                    _residency: Arc::clone(&permit),
                });
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
        shared.publish(ReadOutcome {
            key: job.key,
            generation: job.generation,
            worker,
            priority: job.priority,
            complete: true,
            result,
            _residency: permit,
        });
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
                match Session::connect_with_timeouts(
                    &self.endpoint,
                    Duration::from_secs(1),
                    Duration::from_secs(30),
                ) {
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
            let cancel = self.cancel.clone();
            let interrupt = session.interrupt_handle().ok_or_else(|| {
                ClientError::Io("the local read connection cannot be interrupted safely".to_owned())
            })?;
            let _wake = cancel
                .as_ref()
                .map(|cancel| cancel.on_cancel(move || interrupt.interrupt()));
            if cancel.as_ref().is_some_and(CancellationToken::is_cancelled) {
                self.session = None;
                self.session_epoch = None;
                return Err(ClientError::Io(
                    "read was cancelled before sending".to_owned(),
                ));
            }
            let result = operation(session);
            drop(_wake);
            if cancel.as_ref().is_some_and(CancellationToken::is_cancelled) {
                self.session = None;
                self.session_epoch = None;
                return Err(ClientError::Io(
                    "read was cancelled during transport".to_owned(),
                ));
            }
            match result {
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
            | SurfaceCommand::CargoPackageSourceFile { .. }
            | SurfaceCommand::CargoPackageSourceInventory { .. }
            | SurfaceCommand::CargoPackageReadme { .. }
            | SurfaceCommand::CargoPackageReadmeLink { .. }
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
            ReadRequest::CargoSource(file) => compose_cargo_source(&mut self.engine, file, context),
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
                crate::model::browse::BrowseKey::CargoSourceInventory(key) => {
                    compose_cargo_source_inventory(&mut self.engine, key, context)
                }
                crate::model::browse::BrowseKey::CargoReadme(key) => {
                    super::cargo_readme_reads::compose(&mut self.engine, key, context)
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

pub(super) fn shape(what: &str) -> ReadFailure {
    ReadFailure::Fault(ErrorValue::new(
        FaultCode::Protocol,
        format!("the {what} reply changed shape"),
    ))
}

pub(super) fn check(cancel: &CancellationToken) -> Result<(), ReadFailure> {
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

/// Reads current bytes only through the owner's Cargo metadata receipt.
/// A digest-qualified route is an address; this reply establishes fresh
/// root/file evidence but explicitly does not establish semantic indexing.
fn compose_cargo_source(
    engine: &mut dyn Engine,
    key: &CargoSourceKey,
    context: &ReadContext<'_>,
) -> Result<PageValue, ReadFailure> {
    check(context.cancel)?;
    if matches!(
        key.target,
        crate::navigation::CargoSourceTarget::ReadmeLink(_)
    ) {
        return super::cargo_readme_reads::compose_link(engine, key, context);
    }
    let path = CargoPackageSourcePathV1::new(key.target.path().as_str()).map_err(|_| {
        ReadFailure::Fault(ErrorValue::new(
            FaultCode::Protocol,
            "invalid Cargo source file address",
        ))
    })?;
    let reply = request_cargo_source_file(engine, key, &path)?;
    check(context.cancel)?;
    let SurfaceReply::CargoPackageSourceFile(result) = reply else {
        return Err(shape("Cargo source file"));
    };
    validate_cargo_file_reply(key, &result)?;
    if matches!(
        &result,
        CargoPackageSourceFileResultV1::Unavailable {
            reason: CargoPackageSourceReadFailureV1::AuthorityUnavailable,
            ..
        }
    ) {
        // A cold owner has no prior ProjectTree observation. The retained
        // project is only an address: the owner reads it again, then this
        // worker verifies that its current tree contains the exact qualified
        // package before asking for bytes. No UI path becomes file authority.
        rehydrate_cargo_source_authority(engine, &key.context, &key.package, context.cancel)?;
        check(context.cancel)?;
        let reply = request_cargo_source_file(engine, key, &path)?;
        check(context.cancel)?;
        let SurfaceReply::CargoPackageSourceFile(result) = reply else {
            return Err(shape("Cargo source file"));
        };
        return cargo_source_page(key, result);
    }
    cargo_source_page(key, result)
}

fn request_cargo_source_file(
    engine: &mut dyn Engine,
    key: &CargoSourceKey,
    path: &CargoPackageSourcePathV1,
) -> Result<SurfaceReply, ReadFailure> {
    engine
        .surface(SurfaceCommand::CargoPackageSourceFile {
            request: cargo_source_request(&key.context, &key.package)?,
            path: path.clone(),
        })
        .map_err(|error| failure(&error))
}

fn cargo_source_request(
    context: &crate::navigation::CargoBrowseContext,
    package: &PackageRef,
) -> Result<CargoPackageSourceRequestV1, ReadFailure> {
    let request = CargoPackageSourceRequestV1::from_tree(
        package.reference().clone(),
        context.request_binding(),
    );
    if request.has_admissible_shape() {
        Ok(request)
    } else {
        Err(shape("Cargo source request selector"))
    }
}

pub(super) fn rehydrate_cargo_source_authority(
    engine: &mut dyn Engine,
    browse: &crate::navigation::CargoBrowseContext,
    package: &PackageRef,
    cancel: &CancellationToken,
) -> Result<(), ReadFailure> {
    check(cancel)?;
    let root = browse
        .requested_project()
        .service_coordinate()
        .map_err(|_| {
            ReadFailure::Fault(ErrorValue::new(
                FaultCode::Protocol,
                "This project path cannot be sent to the Cargo owner.",
            ))
        })?;
    let root = ProductText::new(root).map_err(|_| {
        ReadFailure::Fault(ErrorValue::new(
            FaultCode::Protocol,
            "Invalid Cargo project address.",
        ))
    })?;
    let reply = engine
        .surface(SurfaceCommand::ProjectTree { root })
        .map_err(|error| failure(&error))?;
    check(cancel)?;
    let SurfaceReply::ProjectTree(tree) = reply else {
        return Err(shape("Cargo project-tree rehydration"));
    };
    if !tree.has_admissible_shape() {
        return Err(shape("Cargo project-tree proof"));
    }
    if tree.request_binding != Some(browse.request_binding()) {
        return Err(ReadFailure::Fault(ErrorValue::new(
            FaultCode::Missing,
            "This project tree now has a different Cargo browse binding. Reopen its current tree.",
        )));
    }
    if tree.package_by_reference(package.reference()).is_none() {
        return Err(ReadFailure::Fault(ErrorValue::new(
            FaultCode::Missing,
            "This tree does not admit the requested exact Cargo package. Reopen its current observation.",
        )));
    }
    Ok(())
}

/// Admits only exact owner replies and prepares the immutable line index on
/// the read worker. Kept separate from transport for focused proof tests.
fn cargo_source_page(
    key: &CargoSourceKey,
    result: CargoPackageSourceFileResultV1,
) -> Result<PageValue, ReadFailure> {
    validate_cargo_file_reply(key, &result)?;
    match result {
        CargoPackageSourceFileResultV1::Read {
            authority,
            request_binding,
            content_digest,
            contents,
            semantic: CargoPackageSourceSemanticStatusV1::NotIndexed,
            ..
        } => {
            let text = crate::model::pages::SourceText::new(
                Arc::from(contents),
                1,
                crate::model::pages::SourceOrigin::LocalFile,
                true,
            )
            .map_err(|_| shape("Cargo source line range"))?;
            Ok(PageValue::CargoSource(CargoSourcePage {
                package: key.package.clone(),
                request_binding,
                target: key.target.clone(),
                source: text,
                content_digest,
                source_revision: authority.source_revision(),
            }))
        }
        CargoPackageSourceFileResultV1::Stale { .. } => Err(ReadFailure::Fault(ErrorValue::new(
            FaultCode::Missing,
            "The Cargo source changed. Reopen the project tree to get its current files.",
        ))),
        CargoPackageSourceFileResultV1::Unavailable { reason, .. } => {
            let (code, message) = cargo_source_failure(reason);
            Err(ReadFailure::Fault(ErrorValue::new(code, message)))
        }
    }
}

/// A valid submitted selector must be echoed even on a negative reply. A
/// missing selector is not a current owner observation for this request.
fn validate_cargo_file_reply(
    key: &CargoSourceKey,
    result: &CargoPackageSourceFileResultV1,
) -> Result<(), ReadFailure> {
    if key.target.package_file().is_none() {
        return Err(shape("package file scope"));
    }
    if !result.has_admissible_shape() {
        return Err(shape("Cargo source file proof"));
    }
    let exact = match result {
        CargoPackageSourceFileResultV1::Read {
            package,
            request_binding,
            path,
            ..
        } => {
            package == key.package.reference()
                && *request_binding == key.context.request_binding()
                && path.as_str() == key.target.path().as_str()
        }
        CargoPackageSourceFileResultV1::Stale {
            package,
            request_binding,
        } => {
            package == key.package.reference() && *request_binding == key.context.request_binding()
        }
        CargoPackageSourceFileResultV1::Unavailable {
            package,
            request_binding,
            ..
        } => {
            package.as_ref() == Some(key.package.reference())
                && *request_binding == Some(key.context.request_binding())
        }
    };
    if exact {
        Ok(())
    } else {
        Err(shape("Cargo source address mismatch"))
    }
}

fn cargo_source_failure(reason: CargoPackageSourceReadFailureV1) -> (FaultCode, &'static str) {
    use CargoPackageSourceReadFailureV1 as Reason;
    match reason {
        Reason::InvalidPackageReference | Reason::InvalidRelativePath => {
            (FaultCode::Protocol, "This Cargo source address is invalid.")
        }
        Reason::AuthorityUnavailable => (
            FaultCode::Missing,
            "This Cargo package has no current source receipt. Reopen its project tree.",
        ),
        Reason::SourceObservationUnavailable => (
            FaultCode::Missing,
            "The Cargo source observation could not be revalidated completely. Reopen its project tree.",
        ),
        Reason::StaleAuthority => (
            FaultCode::Missing,
            "The Cargo source changed. Reopen its project tree.",
        ),
        Reason::PackageRootUnavailable => (
            FaultCode::Missing,
            "The admitted Cargo source folder is no longer available.",
        ),
        Reason::UnsupportedFileKind => (
            FaultCode::Unsupported,
            "This file kind is not available in the Cargo source reader.",
        ),
        Reason::FileUnavailable => (
            FaultCode::Missing,
            "This source file is absent or cannot be read safely.",
        ),
        Reason::FileTooLarge => (
            FaultCode::Unsupported,
            "This source file is too large for the bounded reader.",
        ),
        Reason::NotUtf8Text => (
            FaultCode::Unsupported,
            "This source file is not UTF-8 text.",
        ),
    }
}

/// Reads navigation hints independently from file bytes. This runs in the
/// read pool and can finish after the file; it never delays the first text.
fn compose_cargo_source_inventory(
    engine: &mut dyn Engine,
    key: &CargoSourceInventoryKey,
    context: &ReadContext<'_>,
) -> Result<PageValue, ReadFailure> {
    check(context.cancel)?;
    let reply = request_cargo_source_inventory(engine, key)?;
    check(context.cancel)?;
    let SurfaceReply::CargoPackageSourceInventory(result) = reply else {
        return Err(shape("Cargo source inventory"));
    };
    validate_cargo_inventory_reply(key, &result)?;
    if matches!(
        &result,
        CargoPackageSourceInventoryResultV1::Unavailable {
            reason: CargoPackageSourceInventoryFailureV1::AuthorityUnavailable,
            ..
        }
    ) {
        rehydrate_cargo_source_authority(engine, &key.context, &key.package, context.cancel)?;
        check(context.cancel)?;
        let reply = request_cargo_source_inventory(engine, key)?;
        check(context.cancel)?;
        let SurfaceReply::CargoPackageSourceInventory(result) = reply else {
            return Err(shape("Cargo source inventory"));
        };
        return cargo_source_inventory_page(key, result);
    }
    cargo_source_inventory_page(key, result)
}

fn request_cargo_source_inventory(
    engine: &mut dyn Engine,
    key: &CargoSourceInventoryKey,
) -> Result<SurfaceReply, ReadFailure> {
    engine
        .surface(SurfaceCommand::CargoPackageSourceInventory {
            request: cargo_source_request(&key.context, &key.package)?,
        })
        .map_err(|error| failure(&error))
}

fn cargo_source_inventory_page(
    key: &CargoSourceInventoryKey,
    result: CargoPackageSourceInventoryResultV1,
) -> Result<PageValue, ReadFailure> {
    validate_cargo_inventory_reply(key, &result)?;
    match result {
        CargoPackageSourceInventoryResultV1::Listed(inventory) => {
            let paths = inventory
                .paths
                .iter()
                .map(|path| {
                    crate::navigation::CargoSourcePath::new(path.as_str())
                        .ok_or_else(|| shape("Cargo source inventory path"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(PageValue::Browse(BrowseValue::CargoSourceInventory(
                Arc::new(CargoSourceInventoryModel {
                    package: key.package.clone(),
                    request_binding: inventory.request_binding,
                    paths: paths.into(),
                    coverage: inventory.coverage,
                    source_revision: inventory.authority.source_revision(),
                }),
            )))
        }
        CargoPackageSourceInventoryResultV1::Stale { .. } => {
            Err(ReadFailure::Fault(ErrorValue::new(
                FaultCode::Missing,
                "The Cargo source changed. Reopen the project tree for its current files.",
            )))
        }
        CargoPackageSourceInventoryResultV1::Unavailable { reason, .. } => {
            let message = match reason {
                CargoPackageSourceInventoryFailureV1::AuthorityUnavailable => {
                    "This Cargo package has no current source receipt. Reopen its project tree."
                }
                CargoPackageSourceInventoryFailureV1::StaleAuthority => {
                    "The Cargo source changed. Reopen its project tree."
                }
                CargoPackageSourceInventoryFailureV1::PackageRootUnavailable => {
                    "The admitted Cargo source folder is no longer available."
                }
                CargoPackageSourceInventoryFailureV1::DirectoryUnavailable => {
                    "The owner could not safely list this Cargo source folder."
                }
            };
            Err(ReadFailure::Fault(ErrorValue::new(
                FaultCode::Missing,
                message,
            )))
        }
    }
}

fn validate_cargo_inventory_reply(
    key: &CargoSourceInventoryKey,
    result: &CargoPackageSourceInventoryResultV1,
) -> Result<(), ReadFailure> {
    if !result.has_admissible_shape() {
        return Err(shape("Cargo source inventory proof"));
    }
    let exact = match result {
        CargoPackageSourceInventoryResultV1::Listed(inventory) => {
            inventory.package == *key.package.reference()
                && inventory.request_binding == key.context.request_binding()
        }
        CargoPackageSourceInventoryResultV1::Stale {
            package,
            request_binding,
        } => {
            package == key.package.reference() && *request_binding == key.context.request_binding()
        }
        CargoPackageSourceInventoryResultV1::Unavailable {
            package,
            request_binding,
            ..
        } => {
            package.as_ref() == Some(key.package.reference())
                && *request_binding == Some(key.context.request_binding())
        }
    };
    if exact {
        Ok(())
    } else {
        Err(shape("Cargo source inventory address mismatch"))
    }
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
    let local = package
        .is_local()
        .then(|| LocalProjectId::from_path(Path::new(package.as_str())).ok())
        .flatten()
        .filter(|project| project.path().is_dir())
        // A registry-shaped reply for this local route may describe a
        // same-name published release. Only the path's own manifest can
        // supply this project's name, version, and license.
        .and_then(|project| loader.load_with_cancel(&project, &|| context.cancel.is_cancelled()));
    check(context.cancel)?;
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
    let verified_registry_releases =
        match (packages.as_ref().ok(), crate::host::registry::composed()) {
            (Some(snapshot), Some(composition)) => {
                verified_registry_releases(snapshot.root.rows(), &composition, context.cancel)?
            }
            _ => std::collections::HashMap::new(),
        };
    check(context.cancel)?;
    Ok(PageValue::Orbit(page_mapping::orbit_model(
        &page_mapping::OrbitInputs {
            packages: packages.as_ref().map(|snapshot| snapshot.root.rows()),
            verified_registry_releases: &verified_registry_releases,
            projects: projects.as_ref(),
            explore: explore.as_ref(),
            tree: tree.as_ref(),
        },
    )))
}

/// Admits exact package identities for local registry trees while this
/// worker still has access to the manifests and the selected registry source.
/// A manifest-shaped directory alone is not a registry membership fact.
fn verified_registry_releases(
    rows: &[Row],
    composition: &crate::host::registry::Composition,
    cancel: &CancellationToken,
) -> Result<std::collections::HashMap<PackageRef, VerifiedRegistryRelease>, ReadFailure> {
    let mut admitted = std::collections::HashMap::new();
    let mut registry_roots = 0;
    for (index, row) in rows.iter().enumerate() {
        if index % 64 == 0 {
            check(cancel)?;
        }
        if !matches!(&row.id, backend_library::RowId::Package(_)) {
            continue;
        }
        let Ok(root) = PackageRef::parse(&row.label) else {
            continue;
        };
        if !root.is_local() || root.registry_shape().is_none() {
            continue;
        }
        registry_roots += 1;
        if registry_roots > MAX_REGISTRY_ROOTS_PER_ORBIT {
            return Ok(std::collections::HashMap::new());
        }
        // Ask the current source for an origin-preserving identity for this
        // exact root. A source unable to represent its origin in a package
        // URL cannot grant a dependency destination. The manifest confirms
        // the identity's name and version; neither path spelling nor
        // Cargo.toml alone is authority.
        let Some(identity) = composition
            .source
            .package_identity_of(Path::new(root.as_str()))
        else {
            continue;
        };
        if root.verify_registry_manifest().as_ref() != Some(&identity.release) {
            continue;
        }
        let admitted_source = root.clone();
        admitted.insert(
            root,
            VerifiedRegistryRelease::from_checked_release(
                admitted_source,
                identity.package,
                Arc::clone(&composition.authority),
                composition.generation.0,
            ),
        );
    }
    Ok(admitted)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::host::registry::{
        Availability, CrateName, Published, RegistryPackageIdentity, SourceError, SourceTree,
    };
    use crate::model::ServiceMode;
    use crate::model::pages::{HealthModel, IngestModel};
    use crate::model::release::Release;
    use crate::runtime::owner::{OwnerFault, OwnerGate, OwnerState};
    use backend_library::{Basis, Row, RowId, object_version, package_key, view_state_root};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    struct ExactRegistryTree {
        root: PathBuf,
        release: Release,
    }

    impl RegistrySource for ExactRegistryTree {
        fn releases(&self, _: &CrateName) -> Vec<Published> {
            Vec::new()
        }

        fn offline(&self, _: &str, _: usize) -> Vec<Release> {
            Vec::new()
        }

        fn availability(&self, release: &Release) -> Availability {
            if release == &self.release {
                Availability::Unpacked(self.root.clone())
            } else {
                Availability::Download
            }
        }

        fn release_of(&self, root: &Path) -> Option<Release> {
            root.canonicalize()
                .is_ok_and(|root| root == self.root)
                .then(|| self.release.clone())
        }

        fn package_identity_of(&self, root: &Path) -> Option<RegistryPackageIdentity> {
            let release = self.release_of(root)?;
            let package = PackageRef::parse(&format!(
                "{}?repository_url=https%3A%2F%2Fregistry.example.test%2Findex",
                release.purl()
            ))
            .ok()?;
            Some(RegistryPackageIdentity { release, package })
        }

        fn resolve(&self, release: &Release) -> Result<SourceTree, SourceError> {
            Err(SourceError::NeedsDownload(release.clone()))
        }
    }

    fn package_row(label: &str) -> Row {
        Row::new(
            RowId::Package(package_key(label)),
            Basis::new(
                view_state_root(&[]),
                object_version(b"orbit-registry-proof"),
            ),
            label,
        )
    }

    /// The Orbit worker binds a local tree to the selected registry source
    /// only after both its manifest and that source's exact root membership
    /// agree. A similarly named local directory remains unproved.
    #[test]
    fn orbit_registry_identity_requires_the_exact_verified_manifest_and_source_root() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);

        let base = std::env::temp_dir().join(format!(
            "nudox-orbit-registry-proof-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        let root = base
            .join("registry")
            .join("src")
            .join("index.test")
            .join("toml-0.8.23");
        std::fs::create_dir_all(&root).expect("registry source root");
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"toml\"\nversion = \"0.8.23\"\n",
        )
        .expect("manifest");
        let other = base.join("workspace").join("toml-0.8.23");
        std::fs::create_dir_all(&other).expect("other local root");
        std::fs::write(
            other.join("Cargo.toml"),
            "[package]\nname = \"toml\"\nversion = \"0.8.23\"\n",
        )
        .expect("other manifest");

        let release = Release::new("toml", "0.8.23").expect("release");
        let source = Arc::new(ExactRegistryTree {
            root: root.canonicalize().expect("canonical source"),
            release: release.clone(),
        });
        let composition = crate::host::registry::Composition {
            endpoint: base.join("owner.sock"),
            source,
            authority: Arc::from("opaque-test-authority"),
            generation: crate::host::registry::CompositionGeneration(43),
            refusals: None,
        };
        let indexed = verified_registry_releases(
            &[
                package_row(root.to_str().expect("source path")),
                package_row(other.to_str().expect("other path")),
            ],
            &composition,
            &CancellationToken::new(),
        )
        .expect("the fixture is not cancelled");
        let proof = indexed
            .get(&PackageRef::parse(root.to_str().expect("source path")).expect("package"))
            .expect("the exact registry member has a proof");
        let purl = PackageRef::parse(&format!(
            "{}?repository_url=https%3A%2F%2Fregistry.example.test%2Findex",
            release.purl()
        ))
        .expect("origin-qualified exact purl");
        let admitted_root =
            PackageRef::parse(root.to_str().expect("source path")).expect("source package");
        assert!(proof.matches(&admitted_root, &purl, "opaque-test-authority", 43,));
        assert!(
            !proof.matches(
                &admitted_root,
                &PackageRef::parse(&release.purl()).expect("unqualified coordinate"),
                "opaque-test-authority",
                43,
            ),
            "the registry origin is part of the dependency identity"
        );
        assert!(
            !proof.matches(
                &admitted_root,
                &PackageRef::parse(&format!(
                    "{}?repository_url=https%3A%2F%2Fother.example%2Findex",
                    release.purl()
                ))
                .expect("other qualified origin"),
                "opaque-test-authority",
                43,
            ),
            "equal name and version from another origin cannot reuse this proof"
        );
        assert!(!proof.matches(
            &PackageRef::parse(other.to_str().expect("other path")).expect("other package"),
            &purl,
            "opaque-test-authority",
            43,
        ));
        assert!(!proof.matches(&admitted_root, &purl, "different-authority", 43,));
        assert!(!proof.matches(&admitted_root, &purl, "opaque-test-authority", 44,));
        assert!(
            !indexed.contains_key(
                &PackageRef::parse(other.to_str().expect("other path")).expect("package")
            ),
            "a matching manifest outside the selected registry source has no proof"
        );

        // The app's own cache nests the release tree below its authority and
        // checksum. It is still a registry root, but only the selected source
        // may attest it.
        let app_root = base
            .join("registry-sources")
            .join("authority-digest")
            .join("toml-0.8.23")
            .join("checksum")
            .join("toml-0.8.23");
        std::fs::create_dir_all(&app_root).expect("app-owned registry source root");
        std::fs::write(
            app_root.join("Cargo.toml"),
            "[package]\nname = \"toml\"\nversion = \"0.8.23\"\n",
        )
        .expect("app-owned manifest");
        let app_source = Arc::new(ExactRegistryTree {
            root: app_root.canonicalize().expect("canonical app source"),
            release: release.clone(),
        });
        let app_composition = crate::host::registry::Composition {
            endpoint: base.join("owner.sock"),
            source: app_source,
            authority: Arc::from("opaque-test-authority"),
            generation: crate::host::registry::CompositionGeneration(44),
            refusals: None,
        };
        let app_indexed = verified_registry_releases(
            &[package_row(app_root.to_str().expect("app source path"))],
            &app_composition,
            &CancellationToken::new(),
        )
        .expect("the fixture is not cancelled");
        let app_package =
            PackageRef::parse(app_root.to_str().expect("app source path")).expect("app package");
        assert!(
            app_indexed.get(&app_package).is_some_and(|proof| {
                proof.matches(&app_package, &purl, "opaque-test-authority", 44)
            }),
            "the exact app-owned cache layout can be admitted by its current registry source"
        );

        std::fs::remove_dir_all(base).expect("remove source fixture");
        assert!(
            PackageRef::parse(root.to_str().expect("source path"))
                .expect("package")
                .verify_registry_manifest()
                .is_none(),
            "removing the source also evicts its memory-only manifest admission"
        );
    }

    #[test]
    fn cargo_source_reply_requires_current_exact_file_proof_and_marks_semantics_unindexed() {
        use backend_library::CargoPackageSourceAuthorityStateV1;
        const METADATA: &[u8] = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../crates/library/browse/fixtures/tree-2026-09-27/metadata.json"
        ));
        let input = backend_library::browse::metadata_input_with_stable_source_witness(
            METADATA,
            "aarch64-apple-darwin",
            None,
            [7; 32],
        )
        .expect("Cargo metadata fixture");
        let row = input
            .packages
            .iter()
            .find(|row| row.name == "serde" && row.version == "1.0.219")
            .expect("resolved package");
        let CargoPackageSourceAuthorityStateV1::Admitted(authority) = &row.source_authority else {
            panic!("resolved metadata must carry exact authority")
        };
        let reference = authority.package_reference().expect("qualified package");
        let path = CargoPackageSourcePathV1::new("Cargo.toml").expect("relative file");
        let project = crate::core::LocalProjectId::new("/tmp/nudox-source-reply").expect("project");
        let binding = backend_library::browse::ProjectTreeRequestBindingV1::for_paths(
            Path::new(project.service_coordinate().expect("coordinate")),
            &input.root,
        )
        .expect("fixture binding");
        let key = CargoSourceKey {
            context: crate::navigation::CargoBrowseContext::from_binding_address(project, binding)
                .expect("bound address"),
            package: PackageRef::from_reference(reference.clone()),
            target: crate::navigation::CargoSourceTarget::PackageFile(
                crate::navigation::CargoSourcePath::new(path.as_str()).expect("GUI path"),
            ),
        };
        let contents: Box<str> = "[package]\nname = \"serde\"\n".into();
        let digest = *blake3::hash(contents.as_bytes()).as_bytes();
        let reply = CargoPackageSourceFileResultV1::Read {
            package: reference.clone(),
            authority: authority.clone(),
            request_binding: binding,
            path: path.clone(),
            content_digest: digest,
            contents: contents.clone(),
            semantic: CargoPackageSourceSemanticStatusV1::NotIndexed,
        };
        let PageValue::CargoSource(page) = cargo_source_page(&key, reply).expect("verified reply")
        else {
            panic!("Cargo file page")
        };
        assert_eq!(page.package, key.package);
        assert_eq!(page.target, key.target);
        assert_eq!(page.content_digest, digest);
        assert_eq!(page.source.text(), contents.as_ref());
        assert_eq!(
            page.source.coverage(),
            crate::model::pages::SourceCoverage::Unverified
        );

        let forged = CargoPackageSourceFileResultV1::Read {
            package: reference.clone(),
            authority: authority.clone(),
            request_binding: binding,
            path: path.clone(),
            content_digest: [0; 32],
            contents,
            semantic: CargoPackageSourceSemanticStatusV1::NotIndexed,
        };
        assert!(
            cargo_source_page(&key, forged).is_err(),
            "a display address cannot authenticate file bytes"
        );
        let mut unrelated = binding;
        unrelated.requested_root_digest = [3; 32];
        for negative in [
            CargoPackageSourceFileResultV1::Stale {
                package: reference.clone(),
                request_binding: unrelated,
            },
            CargoPackageSourceFileResultV1::Unavailable {
                package: Some(reference.clone()),
                request_binding: Some(unrelated),
                reason: CargoPackageSourceReadFailureV1::AuthorityUnavailable,
            },
            CargoPackageSourceFileResultV1::Unavailable {
                package: Some(reference.clone()),
                request_binding: None,
                reason: CargoPackageSourceReadFailureV1::AuthorityUnavailable,
            },
            CargoPackageSourceFileResultV1::Unavailable {
                package: None,
                request_binding: Some(binding),
                reason: CargoPackageSourceReadFailureV1::AuthorityUnavailable,
            },
        ] {
            assert!(
                matches!(cargo_source_page(&key, negative), Err(ReadFailure::Fault(error)) if error.code() == FaultCode::Protocol),
                "a negative reply must echo the exact submitted package and complete binding"
            );
        }
        assert!(
            matches!(cargo_source_page(&key, CargoPackageSourceFileResultV1::Stale { package: reference, request_binding: binding }),
            Err(ReadFailure::Fault(error)) if error.code() == FaultCode::Missing)
        );
    }

    #[test]
    fn cold_cargo_file_rehydrates_only_the_tree_containing_its_exact_authority() {
        use backend_advisory::{AdvisoryAuthority, normalize_package};
        use backend_library::CargoPackageSourceAuthorityStateV1;
        use backend_library::browse::{
            ProjectTree, build_tree, metadata_input_with_stable_source_witness,
        };

        const METADATA: &[u8] = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../crates/library/browse/fixtures/tree-2026-09-27/metadata.json"
        ));
        const LOCKFILE: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../crates/library/browse/fixtures/tree-2026-09-27/Cargo.lock"
        ));
        let input = metadata_input_with_stable_source_witness(
            METADATA,
            "aarch64-apple-darwin",
            Some(LOCKFILE),
            [7; 32],
        )
        .expect("Cargo metadata fixture");
        let advisories = AdvisoryAuthority::new(1);
        let observe = |name: &str, version: &str| {
            let package = normalize_package("cargo", name).expect("identity");
            advisories.observe(&package, version, false, false, 0, false)
        };
        let mut tree = build_tree(&input, &observe);
        let requested_project =
            LocalProjectId::new("/workspace/backend/member").expect("requested member");
        let binding = backend_library::browse::ProjectTreeRequestBindingV1::for_paths(
            Path::new(requested_project.service_coordinate().expect("coordinate")),
            &tree.root,
        )
        .expect("owner fixture binding");
        tree.request_binding = Some(binding);
        let row = tree.package("serde", "1.0.219").expect("exact row");
        let CargoPackageSourceAuthorityStateV1::Admitted(authority) = &row.source_authority else {
            panic!("metadata receipt")
        };
        let package = authority.package_reference().expect("qualified package");
        let path = CargoPackageSourcePathV1::new("Cargo.toml").expect("path");
        let contents: Box<str> = "[package]\nname = \"serde\"\n".into();
        let digest = *blake3::hash(contents.as_bytes()).as_bytes();
        let current = CargoPackageSourceFileResultV1::Read {
            package: package.clone(),
            authority: authority.clone(),
            request_binding: binding,
            path: path.clone(),
            content_digest: digest,
            contents,
            semantic: CargoPackageSourceSemanticStatusV1::NotIndexed,
        };
        struct ColdEngine {
            tree: ProjectTree,
            current: CargoPackageSourceFileResultV1,
            seen: Vec<&'static str>,
        }
        impl Engine for ColdEngine {
            fn revision(&mut self) -> Result<ViewStateRoot, ClientError> {
                Err(ClientError::Protocol("unused".to_owned()))
            }
            fn health(&mut self) -> Result<HealthReport, ClientError> {
                Err(ClientError::Protocol("unused".to_owned()))
            }
            fn probe(&mut self, _: Probe<'_>) -> Result<ReplyDto, ClientError> {
                Err(ClientError::Protocol("unused".to_owned()))
            }
            fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
                match command {
                    SurfaceCommand::CargoPackageSourceFile { request, .. }
                        if self.seen.is_empty() =>
                    {
                        self.seen.push("file");
                        Ok(SurfaceReply::CargoPackageSourceFile(
                            CargoPackageSourceFileResultV1::Unavailable {
                                package: Some(request.package),
                                request_binding: Some(request.request_binding),
                                reason: CargoPackageSourceReadFailureV1::AuthorityUnavailable,
                            },
                        ))
                    }
                    SurfaceCommand::ProjectTree { root } => {
                        assert_eq!(root.as_str(), "/workspace/backend/member");
                        self.seen.push("tree");
                        Ok(SurfaceReply::ProjectTree(Box::new(self.tree.clone())))
                    }
                    SurfaceCommand::CargoPackageSourceFile { request, path } => {
                        assert_eq!(path.as_str(), "Cargo.toml");
                        self.seen.push("file");
                        if let CargoPackageSourceFileResultV1::Read {
                            package: current, ..
                        } = &self.current
                        {
                            assert_eq!(&request.package, current);
                            assert_eq!(Some(request.request_binding), self.tree.request_binding);
                        }
                        Ok(SurfaceReply::CargoPackageSourceFile(self.current.clone()))
                    }
                    _ => Err(ClientError::Protocol("unexpected command".to_owned())),
                }
            }
        }

        let key = CargoSourceKey {
            context: crate::navigation::CargoBrowseContext::from_binding_address(
                requested_project,
                binding,
            )
            .expect("bound member"),
            package: PackageRef::from_reference(package),
            target: crate::navigation::CargoSourceTarget::PackageFile(
                crate::navigation::CargoSourcePath::new(path.as_str()).expect("path"),
            ),
        };
        let cancel = CancellationToken::new();
        let outlines = OutlineCache::default();
        let context = ReadContext {
            worker: 0,
            cancel: &cancel,
            outlines: &outlines,
            progress: None,
        };
        let mut engine = ColdEngine {
            tree,
            current,
            seen: Vec::new(),
        };
        assert!(matches!(
            compose_cargo_source(&mut engine, &key, &context),
            Ok(PageValue::CargoSource(_))
        ));
        assert_eq!(engine.seen, ["file", "tree", "file"]);

        let mut changed_tree = engine.tree.clone();
        changed_tree
            .request_binding
            .as_mut()
            .expect("binding")
            .requested_root_digest = [4; 32];
        let mut changed = ColdEngine {
            tree: changed_tree,
            current: engine.current.clone(),
            seen: Vec::new(),
        };
        assert!(
            matches!(compose_cargo_source(&mut changed, &key, &context), Err(ReadFailure::Fault(error)) if error.code() == FaultCode::Missing)
        );
        assert_eq!(
            changed.seen,
            ["file", "tree"],
            "a different requested Tree binding cannot trigger a retry"
        );

        let other = CargoSourceKey {
            package: PackageRef::parse("pkg:cargo/serde@1.0.219?cargo-authority=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
                .expect("other authority"),
            ..key
        };
        let mut engine = ColdEngine {
            tree: engine.tree,
            current: engine.current,
            seen: Vec::new(),
        };
        assert!(
            matches!(compose_cargo_source(&mut engine, &other, &context), Err(ReadFailure::Fault(error)) if error.code() == FaultCode::Missing)
        );
        assert_eq!(
            engine.seen,
            ["file", "tree"],
            "an unrelated tree cannot trigger a file retry"
        );
    }

    #[test]
    fn cargo_inventory_preserves_exact_paths_and_partial_coverage_without_file_proof() {
        use backend_library::{
            CargoPackageSourceAuthorityStateV1, CargoPackageSourceInventoryCoverageV1,
            CargoPackageSourceInventoryGapV1, CargoPackageSourceInventoryV1,
        };
        const METADATA: &[u8] = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../crates/library/browse/fixtures/tree-2026-09-27/metadata.json"
        ));
        let input = backend_library::browse::metadata_input_with_stable_source_witness(
            METADATA,
            "aarch64-apple-darwin",
            None,
            [7; 32],
        )
        .expect("Cargo metadata fixture");
        let row = input
            .packages
            .iter()
            .find(|row| row.name == "serde" && row.version == "1.0.219")
            .expect("resolved package");
        let CargoPackageSourceAuthorityStateV1::Admitted(authority) = &row.source_authority else {
            panic!("source receipt")
        };
        let package = authority.package_reference().expect("qualified package");
        let project = LocalProjectId::new("/workspace/backend/member").expect("requested member");
        let binding = backend_library::browse::ProjectTreeRequestBindingV1::for_paths(
            Path::new(project.service_coordinate().expect("coordinate")),
            &input.root,
        )
        .expect("owner fixture binding");
        let key = CargoSourceInventoryKey {
            context: crate::navigation::CargoBrowseContext::from_binding_address(project, binding)
                .expect("bound member"),
            package: PackageRef::from_reference(package.clone()),
        };
        let paths = ["Cargo.toml", "src/lib.rs"]
            .into_iter()
            .map(|path| CargoPackageSourcePathV1::new(path).expect("path"))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let inventory = CargoPackageSourceInventoryV1 {
            package: package.clone(),
            authority: authority.clone(),
            request_binding: binding,
            paths,
            coverage: CargoPackageSourceInventoryCoverageV1::Partial {
                reason: CargoPackageSourceInventoryGapV1::DirectoryUnavailable,
            },
        };
        let PageValue::Browse(BrowseValue::CargoSourceInventory(model)) =
            cargo_source_inventory_page(
                &key,
                CargoPackageSourceInventoryResultV1::Listed(inventory.clone()),
            )
            .expect("exact inventory")
        else {
            panic!("inventory page")
        };
        assert_eq!(
            model
                .paths
                .iter()
                .map(|path| path.as_str())
                .collect::<Vec<_>>(),
            ["Cargo.toml", "src/lib.rs"]
        );
        assert_eq!(model.coverage, inventory.coverage);
        assert_eq!(model.source_revision, authority.source_revision());

        let mut unrelated = binding;
        unrelated.requested_root_digest = [3; 32];
        for negative in [
            CargoPackageSourceInventoryResultV1::Stale {
                package: package.clone(),
                request_binding: unrelated,
            },
            CargoPackageSourceInventoryResultV1::Unavailable {
                package: Some(package.clone()),
                request_binding: Some(unrelated),
                reason: CargoPackageSourceInventoryFailureV1::AuthorityUnavailable,
            },
            CargoPackageSourceInventoryResultV1::Unavailable {
                package: Some(package.clone()),
                request_binding: None,
                reason: CargoPackageSourceInventoryFailureV1::AuthorityUnavailable,
            },
        ] {
            assert!(
                matches!(cargo_source_inventory_page(&key, negative), Err(ReadFailure::Fault(error)) if error.code() == FaultCode::Protocol)
            );
        }

        let mut wrong = inventory.clone();
        wrong.paths.swap(0, 1);
        assert!(
            cargo_source_inventory_page(&key, CargoPackageSourceInventoryResultV1::Listed(wrong))
                .is_err(),
            "a wire list with unverified ordering is not a navigable file index"
        );
        let other = CargoSourceInventoryKey {
            package: PackageRef::parse("pkg:cargo/serde@1.0.219?cargo-authority=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
                .expect("other source"),
            ..key
        };
        assert!(
            cargo_source_inventory_page(
                &other,
                CargoPackageSourceInventoryResultV1::Listed(inventory)
            )
            .is_err(),
            "paths from a different source cannot be lent to this route"
        );
    }

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

    #[cfg(unix)]
    #[test]
    fn closing_the_read_pool_interrupts_an_active_authenticated_socket_read() {
        use std::io::Read as _;
        use std::os::unix::fs::PermissionsExt as _;
        use std::os::unix::net::UnixListener;

        let path = std::path::PathBuf::from(format!(
            "/tmp/nudox-page-interrupt-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos(),
        ));
        let listener = UnixListener::bind(&path).expect("private socket");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("private endpoint");
        let (entered, received) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accepted client");
            let mut frame_length = [0_u8; 4];
            socket
                .read_exact(&mut frame_length)
                .expect("client sent request");
            entered.send(()).expect("request reached server");
            let _ = released.recv_timeout(Duration::from_secs(3));
        });
        let worker_path = path.clone();
        let pool =
            ReadPool::start(1, move |_| SessionReader::connect(&worker_path)).expect("read pool");
        assert!(pool.submit(ReadJob {
            key: PageKey::Health,
            request: ReadRequest::Health,
            generation: Generation::new(1),
            priority: Priority::Normal,
            cancel: CancellationToken::new(),
            affinity: None,
        }));
        received
            .recv_timeout(Duration::from_secs(2))
            .expect("page read entered socket");
        let (closed, finished) = mpsc::channel();
        std::thread::spawn(move || closed.send(drop(pool)).expect("pool closed"));
        finished
            .recv_timeout(Duration::from_secs(1))
            .expect("active socket read held pool shutdown");
        release.send(()).expect("release server");
        server.join().expect("server stopped");
        std::fs::remove_file(path).expect("remove endpoint");
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
            not_ready_capabilities: Arc::from([]),
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
    fn bounded_read_completed_reads_and_drained_clones_hold_admission_until_drop() {
        let harness = harness(2);
        for round in 0..MAX_ADMITTED_READS {
            assert!(harness.pool.submit(job(
                &format!("fast-{round}"),
                round as u64,
                Priority::Normal
            )));
        }
        for _ in 0..MAX_ADMITTED_READS {
            harness.started();
        }
        crate::runtime::wait::until("all results published without a UI consumer", || {
            harness
                .pool
                .shared
                .results
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .len()
                == MAX_ADMITTED_READS
        });
        assert!(!harness.pool.submit(job("overflow", 100, Priority::Normal)));
        assert_eq!(
            harness.pool.shared.admitted.load(Ordering::Acquire),
            MAX_ADMITTED_READS
        );
        let batch = harness.pool.drain();
        assert_eq!(batch.len(), LANDING_BUDGET);
        assert!(
            !harness
                .pool
                .submit(job("still-full", 101, Priority::Normal)),
            "draining is not landing/dropping"
        );
        let held = batch[0].clone();
        drop(batch);
        assert_eq!(
            harness.pool.shared.admitted.load(Ordering::Acquire),
            MAX_ADMITTED_READS - LANDING_BUDGET + 1
        );
        drop(held);
        assert_eq!(
            harness.pool.shared.admitted.load(Ordering::Acquire),
            MAX_ADMITTED_READS - LANDING_BUDGET
        );
        for round in 0..LANDING_BUDGET {
            assert!(harness.pool.submit(job(
                &format!("refill-{round}"),
                200 + round as u64,
                Priority::Normal
            )));
        }
        assert!(
            !harness
                .pool
                .submit(job("full-again", 300, Priority::Normal))
        );
        let count = Arc::clone(&harness.pool.shared.admitted);
        drop(harness);
        assert_eq!(
            count.load(Ordering::Acquire),
            0,
            "shutdown releases queued and published work"
        );
    }

    #[test]
    fn bounded_read_prefetch_reserves_normal_admission_and_queued_cancellation_releases_it() {
        let harness = harness(1);
        assert!(harness.pool.submit(job("slow-busy", 1, Priority::Normal)));
        harness.started();
        for round in 1..MAX_PREFETCH_ADMISSION {
            assert!(harness.pool.submit(job(
                &format!("prefetch-{round}"),
                round as u64,
                Priority::Prefetch
            )));
        }
        assert!(
            !harness
                .pool
                .submit(job("prefetch-overflow", 100, Priority::Prefetch))
        );
        for round in MAX_PREFETCH_ADMISSION..MAX_ADMITTED_READS {
            assert!(harness.pool.submit(job(
                &format!("visible-{round}"),
                round as u64,
                Priority::Normal
            )));
        }
        assert!(
            !harness
                .pool
                .submit(job("visible-overflow", 200, Priority::Normal))
        );
        assert!(harness.pool.cancel(&key("prefetch-1")));
        assert!(
            harness
                .pool
                .submit(job("visible-replacement", 201, Priority::Normal))
        );
        harness.release("slow-busy");
        assert_eq!(
            harness.started().1,
            format!("visible-{MAX_PREFETCH_ADMISSION}"),
            "normal work runs before queued prefetches"
        );
    }

    #[test]
    fn bounded_read_partial_flood_coalesces_and_cancelled_terminal_keeps_the_drained_permit() {
        struct FloodReader {
            staged: mpsc::Sender<()>,
            finish: Arc<(Mutex<bool>, Condvar)>,
        }
        impl PageReader for FloodReader {
            fn read(
                &mut self,
                _: &ReadRequest,
                context: &ReadContext<'_>,
            ) -> Result<PageValue, ReadFailure> {
                for rows in 0..1_000 {
                    let mut page = health();
                    page.rows = rows;
                    context.publish(PageValue::Health(page));
                }
                self.staged.send(()).expect("flood published");
                let (lock, ready) = &*self.finish;
                let mut done = lock.lock().unwrap_or_else(PoisonError::into_inner);
                while !*done {
                    done = ready.wait(done).unwrap_or_else(PoisonError::into_inner);
                }
                // The pool must replace success with cancellation even when a
                // reader finishes late without cooperating with its token.
                Ok(PageValue::Health(health()))
            }
        }
        let (staged, receiver) = mpsc::channel();
        let finish = Arc::new((Mutex::new(false), Condvar::new()));
        let mut pool = ReadPool::start(1, |_| FloodReader {
            staged: staged.clone(),
            finish: Arc::clone(&finish),
        })
        .expect("pool");
        let mut wake = pool.take_wake().expect("wake");
        let pending = job("flood", 1, Priority::Normal);
        let cancel = pending.cancel.clone();
        assert!(pool.submit(pending));
        receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("all partials published");
        assert!(wake.try_take());
        assert!(
            !wake.try_take(),
            "a thousand publications leave one pending wake"
        );
        let partial = pool.drain();
        assert_eq!(partial.len(), 1);
        assert!(matches!(&partial[0].result, Ok(PageValue::Health(page)) if page.rows == 999));
        cancel.cancel();
        {
            let (lock, ready) = &*finish;
            *lock.lock().unwrap_or_else(PoisonError::into_inner) = true;
            ready.notify_all();
        }
        crate::runtime::wait::until("cancelled terminal published", || {
            !pool
                .shared
                .results
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty()
        });
        let terminal = pool.drain();
        assert_eq!(terminal.len(), 1);
        assert!(terminal[0].complete);
        assert_eq!(terminal[0].result, Err(ReadFailure::Cancelled));
        drop(terminal);
        assert_eq!(
            pool.shared.admitted.load(Ordering::Acquire),
            1,
            "the drained partial still owns the admission"
        );
        drop(partial);
        assert_eq!(pool.shared.admitted.load(Ordering::Acquire), 0);
    }

    #[test]
    fn bounded_read_a_terminal_replaces_its_queued_partial_without_losing_failure() {
        let harness = harness(1);
        let permit =
            ReadPermit::acquire(&harness.pool.shared.admitted, Priority::Normal).expect("permit");
        let outcome = |complete, result| ReadOutcome {
            key: PageKey::Health,
            generation: Generation::new(1),
            worker: 0,
            priority: Priority::Normal,
            complete,
            result,
            _residency: Arc::clone(&permit),
        };
        harness
            .pool
            .shared
            .publish(outcome(false, Ok(PageValue::Health(health()))));
        harness
            .pool
            .shared
            .publish(outcome(true, Err(ReadFailure::Cancelled)));
        // A late partial cannot displace a queued terminal.
        harness
            .pool
            .shared
            .publish(outcome(false, Ok(PageValue::Health(health()))));
        let batch = harness.pool.drain();
        assert_eq!(batch.len(), 1);
        assert!(batch[0].complete);
        assert_eq!(batch[0].result, Err(ReadFailure::Cancelled));
    }

    #[test]
    fn bounded_read_bounded_batches_rearm_until_empty_without_losing_a_concurrent_publish() {
        let mut harness = harness(2);
        let mut wake = harness.pool.take_wake().expect("wake");
        for round in 0..25 {
            assert!(
                harness
                    .pool
                    .submit(job(&format!("fast-{round}"), round, Priority::Normal))
            );
        }
        for _ in 0..25 {
            harness.started();
        }
        crate::runtime::wait::until("25 queued terminals", || {
            harness
                .pool
                .shared
                .results
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .len()
                == 25
        });
        let mut landed = 0;
        for expected in [8, 8, 8, 1] {
            assert!(wake.try_take(), "remaining work retained its wake");
            let batch = harness.pool.drain();
            assert_eq!(batch.len(), expected);
            landed += batch.len();
            drop(batch);
        }
        assert_eq!(landed, 25);
        assert!(!wake.try_take(), "empty queue schedules no idle turn");
        assert!(
            harness
                .pool
                .submit(job("after-empty", 26, Priority::Normal))
        );
        harness.started();
        crate::runtime::wait::until("post-drain publication wakes again", || wake.try_take());
        assert_eq!(harness.pool.drain().len(), 1);
    }

    #[test]
    fn bounded_read_visible_delivery_precedes_prefetch_without_starving_it() {
        let harness = harness(1);
        // Publish prefetches first: completion order cannot bury the visible
        // route. A full normal burst must still make prefetch progress.
        for priority in [Priority::Prefetch, Priority::Normal] {
            for round in 0..14 {
                let permit = ReadPermit::acquire(&harness.pool.shared.admitted, priority)
                    .expect("admission");
                harness.pool.shared.publish(ReadOutcome {
                    key: key(&format!("{priority:?}-{round}")),
                    generation: Generation::new(round),
                    worker: 0,
                    priority,
                    complete: true,
                    result: Ok(PageValue::Health(health())),
                    _residency: permit,
                });
            }
        }
        for batch_index in 0..2 {
            let batch = harness.pool.drain();
            assert_eq!(batch.len(), LANDING_BUDGET);
            assert!(
                batch[..7]
                    .iter()
                    .all(|outcome| outcome.priority == Priority::Normal)
            );
            assert_eq!(
                batch[7].priority,
                Priority::Prefetch,
                "one prefetch progresses in each full mixed batch"
            );
            assert_eq!(
                batch[0].key,
                key(&format!("Normal-{}", batch_index * 7)),
                "FIFO within normal priority"
            );
            assert_eq!(
                batch[7].key,
                key(&format!("Prefetch-{batch_index}")),
                "FIFO within prefetch priority"
            );
        }
        assert!(
            harness
                .pool
                .drain()
                .iter()
                .all(|outcome| outcome.priority == Priority::Prefetch)
        );
    }

    #[test]
    fn bounded_read_a_completed_prefetch_becoming_visible_leads_without_worker_mutation() {
        let harness = harness(1);
        for round in 0..12 {
            let permit = ReadPermit::acquire(&harness.pool.shared.admitted, Priority::Prefetch)
                .expect("prefetch admission");
            harness.pool.shared.publish(ReadOutcome {
                key: key(&format!("prefetch-{round}")),
                generation: Generation::new(round),
                worker: 0,
                priority: Priority::Prefetch,
                complete: true,
                result: Ok(PageValue::Health(health())),
                _residency: permit,
            });
        }
        let visible = key("prefetch-11");
        let batch = harness.pool.drain_for(&BTreeSet::from([visible.clone()]));
        assert_eq!(batch.len(), LANDING_BUDGET);
        assert_eq!(
            batch[0].key, visible,
            "current selection overtakes earlier completed prefetches"
        );
        assert_eq!(
            batch[0].priority,
            Priority::Prefetch,
            "the actual scheduling history is preserved"
        );
        assert_eq!(
            batch[1].key,
            key("prefetch-0"),
            "other results retain FIFO order"
        );
    }

    #[test]
    fn bounded_read_a_running_prefetch_uses_latest_visible_selection_at_landing() {
        let harness = harness(1);
        assert!(
            harness
                .pool
                .submit(job("slow-visible", 77, Priority::Prefetch))
        );
        assert_eq!(harness.started().1, "slow-visible");
        for round in 0..12 {
            let permit = ReadPermit::acquire(&harness.pool.shared.admitted, Priority::Prefetch)
                .expect("prefetch admission");
            harness.pool.shared.publish(ReadOutcome {
                key: key(&format!("earlier-{round}")),
                generation: Generation::new(round),
                worker: 0,
                priority: Priority::Prefetch,
                complete: true,
                result: Ok(PageValue::Health(health())),
                _residency: permit,
            });
        }
        let visible = BTreeSet::from([key("slow-visible")]);
        assert!(
            !harness.pool.promote(&key("slow-visible")),
            "running work needs no queue mutation"
        );
        harness.release("slow-visible");
        crate::runtime::wait::until("visible prefetch completed after focus changed", || {
            harness
                .pool
                .shared
                .results
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .len()
                == 13
        });
        let batch = harness.pool.drain_for(&visible);
        assert_eq!(batch[0].key, key("slow-visible"));
        assert_eq!(batch[0].generation, Generation::new(77));
        assert_eq!(batch[0].priority, Priority::Prefetch);
        assert_eq!(batch[1].key, key("earlier-0"));
    }

    #[test]
    fn bounded_read_cancellation_callbacks_reenter_after_queue_changes() {
        let harness = harness(1);
        let pool = Arc::new(harness.pool);
        let (observed, events) = mpsc::channel();
        let callback = |token: &CancellationToken| {
            let shared = Arc::downgrade(&pool.shared);
            let read_pool = Arc::downgrade(&pool);
            let observed = observed.clone();
            token.on_cancel(move || {
                let shared = shared
                    .upgrade()
                    .expect("pool state remains during cancellation");
                let queue = shared
                    .queue
                    .try_lock()
                    .expect("callback must not inherit the queue lock");
                let state = (queue.jobs.len(), queue.closed);
                drop(queue);
                if let Some(pool) = read_pool.upgrade() {
                    assert_eq!(
                        pool.queued(),
                        state.0,
                        "actual pool query can reenter cancellation"
                    );
                }
                observed.send(state).expect("callback observation");
            })
        };
        let pinned = |generation| {
            let mut job = job("never-started", generation, Priority::Normal);
            job.affinity = Some(usize::MAX);
            job
        };
        let first = pinned(1);
        let first_wake = callback(&first.cancel);
        assert!(pool.submit(first));
        let second = pinned(2);
        let second_wake = callback(&second.cancel);
        assert!(pool.submit(second));
        assert_eq!(
            events
                .recv_timeout(Duration::from_secs(1))
                .expect("replacement callback"),
            (1, false)
        );
        assert!(pool.cancel(&key("never-started")));
        assert_eq!(
            events
                .recv_timeout(Duration::from_secs(1))
                .expect("cancel callback"),
            (0, false)
        );
        let third = pinned(3);
        let third_wake = callback(&third.cancel);
        assert!(pool.submit(third));
        drop(pool);
        assert_eq!(
            events
                .recv_timeout(Duration::from_secs(1))
                .expect("shutdown callback"),
            (0, true)
        );
        drop((first_wake, second_wake, third_wake));
    }

    #[test]
    fn bounded_read_large_source_and_late_generation_keep_exact_page_admission() {
        use crate::core::VersionedRoot;
        use crate::model::pages::{
            DeclRef, Known, Landing, PageStore, SourceOrigin, SourceText, SourceView,
        };
        struct SourceReader(Arc<str>);
        impl PageReader for SourceReader {
            fn read(
                &mut self,
                request: &ReadRequest,
                context: &ReadContext<'_>,
            ) -> Result<PageValue, ReadFailure> {
                let ReadRequest::Source(symbol) = request else {
                    panic!("source request")
                };
                let unknown = || Gap::new(GapReason::NotCaptured, "fixture source");
                let page = PageValue::Source(SourceView {
                    symbol: DeclRef::from_label(symbol.as_str(), None, None, None)
                        .expect("declaration"),
                    file: Known::Unknown(unknown()),
                    editor_path: Known::Unknown(unknown()),
                    text: Known::Known(
                        SourceText::new(Arc::clone(&self.0), 1, SourceOrigin::LocalFile, true)
                            .expect("4MiB source"),
                    ),
                    declaration: Known::Unknown(unknown()),
                    identifiers: Known::Unknown(unknown()),
                    uses: Known::Unknown(unknown()),
                    uses_elsewhere: Arc::from([]),
                });
                context.publish(page.clone());
                Ok(page)
            }
        }
        let text: Arc<str> = Arc::from("x".repeat(MAX_LOCAL_SOURCE_FILE_BYTES as usize));
        let pool = ReadPool::start(1, |_| SourceReader(Arc::clone(&text))).expect("pool");
        let symbol = SymbolRef::new("large-source").expect("symbol");
        let key = PageKey::Source(symbol.clone());
        let root = |n: u64| {
            VersionedRoot::synthetic(view_state_root(&[("source".to_owned(), n.to_string())]), n)
        };
        let mut pages = PageStore::default();
        let old = pages.begin(&key, root(1)).expect("old generation");
        let submit = |generation| {
            assert!(pool.submit(ReadJob {
                key: key.clone(),
                request: ReadRequest::Source(symbol.clone()),
                generation,
                priority: Priority::Normal,
                cancel: CancellationToken::new(),
                affinity: None,
            }))
        };
        submit(old);
        crate::runtime::wait::until("old source completed but not landed", || {
            pool.shared
                .results
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .any(|outcome| outcome.complete)
        });
        assert!(pages.revoke_owner_read(&key));
        let current = pages.begin(&key, root(2)).expect("new owner generation");
        submit(current);
        crate::runtime::wait::until("both terminal generations queued", || {
            pool.shared
                .results
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .filter(|outcome| outcome.complete)
                .count()
                == 2
        });
        let outcomes = pool.drain();
        assert_eq!(
            outcomes.len(),
            2,
            "terminal replaced each queued source partial"
        );
        for outcome in outcomes {
            let expected = if outcome.generation == old {
                Landing::Superseded
            } else {
                Landing::Applied
            };
            assert_eq!(
                pages.land(&outcome.key, outcome.generation, outcome.result),
                expected
            );
        }
        let source = pages.source(&symbol);
        assert_eq!(source.value_root(), Some(root(2)));
        let Known::Known(source) = &source.loaded_value().expect("current source").text else {
            panic!("source text")
        };
        assert_eq!(source.text().len(), MAX_LOCAL_SOURCE_FILE_BYTES as usize);
        assert!(
            std::ptr::eq(source.text(), text.as_ref()),
            "worker and landing share the immutable bytes"
        );
        assert_eq!(
            pool.shared.admitted.load(Ordering::Acquire),
            0,
            "landing released both work permits"
        );
    }

    #[test]
    fn bounded_read_shutdown_interrupts_active_publication_and_releases_pending_models() {
        struct CancelReader {
            staged: mpsc::Sender<()>,
            gate: Arc<(Mutex<()>, Condvar)>,
        }
        impl PageReader for CancelReader {
            fn read(
                &mut self,
                _: &ReadRequest,
                context: &ReadContext<'_>,
            ) -> Result<PageValue, ReadFailure> {
                let gate = Arc::clone(&self.gate);
                let _wake = context.cancel.on_cancel(move || {
                    let _guard = gate.0.lock().unwrap_or_else(PoisonError::into_inner);
                    gate.1.notify_all();
                });
                context.publish(PageValue::Health(health()));
                self.staged.send(()).expect("partial queued");
                let mut guard = self.gate.0.lock().unwrap_or_else(PoisonError::into_inner);
                while !context.cancel.is_cancelled() {
                    guard = self
                        .gate
                        .1
                        .wait(guard)
                        .unwrap_or_else(PoisonError::into_inner);
                }
                Err(ReadFailure::Cancelled)
            }
        }
        let (staged, published) = mpsc::channel();
        let gate = Arc::new((Mutex::new(()), Condvar::new()));
        let pool = ReadPool::start(1, |_| CancelReader {
            staged: staged.clone(),
            gate: Arc::clone(&gate),
        })
        .expect("pool");
        assert!(pool.submit(job("active", 1, Priority::Normal)));
        published
            .recv_timeout(Duration::from_secs(2))
            .expect("partial publication");
        assert!(pool.submit(job("queued", 2, Priority::Normal)));
        let count = Arc::clone(&pool.shared.admitted);
        let (closed, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            drop(pool);
            closed.send(()).expect("closed");
        });
        receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("shutdown does not wait for a result consumer");
        assert_eq!(count.load(Ordering::Acquire), 0);
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
