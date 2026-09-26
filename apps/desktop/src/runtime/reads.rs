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
use super::page_mapping::{self, OutlineIndex, PackageInputs, SymbolInputs};
use super::wake::{WakeReceiver, WakeSender, wake_channel};
use crate::core::{ErrorValue, FaultCode, LocalProjectId};
use crate::model::local_package::LocalPackageLoader;
use crate::model::pages::{
    Gap, GapReason, PackageRef, PageKey, PageValue, ReadFailure, SearchContinuation, SearchQuery,
    SymbolRef,
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
/// Upper bound on outline pages read for one package (40 000 rows).
const OUTLINE_PAGES: usize = 200;
/// Explore page size for the Orbit catalog.
const EXPLORE_LIMIT: u16 = 64;

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
    pub generation: u64,
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
    pub generation: u64,
    /// Worker that ran it.
    pub worker: usize,
    /// Scheduling class it ran at.
    pub priority: Priority,
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
    fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure>;
}

/// Worker-side context for one read.
#[derive(Debug)]
pub struct ReadContext<'a> {
    /// Worker index (for continuation affinity).
    pub worker: usize,
    /// Cancellation token of the running job.
    pub cancel: &'a CancellationToken,
    /// Package outlines shared by every worker.
    pub outlines: &'a OutlineCache,
}

/// One cached outline: package, revision root, index.
type OutlineEntry = (PackageRef, ViewStateRoot, Arc<OutlineIndex>);

/// Package outlines shared across workers, keyed by package and revision.
#[derive(Debug, Default)]
pub struct OutlineCache {
    entries: Mutex<VecDeque<OutlineEntry>>,
}

impl OutlineCache {
    const CAPACITY: usize = 8;

    /// Returns a cached outline for `package` at `root`.
    #[must_use]
    pub fn get(&self, package: &PackageRef, root: ViewStateRoot) -> Option<Arc<OutlineIndex>> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let position = entries
            .iter()
            .position(|(cached, at, _)| cached == package && *at == root)?;
        let entry = entries.remove(position)?;
        let index = Arc::clone(&entry.2);
        entries.push_front(entry);
        Some(index)
    }

    /// Stores an outline, evicting the least recently used.
    pub fn put(&self, package: PackageRef, root: ViewStateRoot, index: Arc<OutlineIndex>) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.retain(|(cached, _, _)| *cached != package);
        entries.push_front((package, root, index));
        entries.truncate(Self::CAPACITY);
    }
}

/// The job one worker is running: its key, generation, and token.
type Running = Option<(PageKey, u64, CancellationToken)>;

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
    pub fn submit(&self, job: ReadJob) {
        let mut queue = self.shared.queue();
        if queue.closed {
            return;
        }
        queue.jobs.retain(|queued| {
            if queued.key == job.key {
                queued.cancel.cancel();
                false
            } else {
                true
            }
        });
        for (key, generation, token) in queue.running.iter().flatten() {
            if *key == job.key && *generation != job.generation {
                token.cancel();
            }
        }
        queue.jobs.push_back(job);
        drop(queue);
        self.shared.ready.notify_all();
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
        for (running, _, token) in queue.running.iter().flatten() {
            if running == key {
                token.cancel();
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
            for (_, _, token) in queue.running.iter().flatten() {
                token.cancel();
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
                        *slot = Some((job.key.clone(), job.generation, job.cancel.clone()));
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
            let context = ReadContext {
                worker,
                cancel: &job.cancel,
                outlines: &shared.outlines,
            };
            // A panicking reader must not take the worker (and every later
            // read) down with it; it becomes one typed fault.
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
        let result = match result {
            Ok(_) if job.cancel.is_cancelled() => Err(ReadFailure::Cancelled),
            other => other,
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
        }
    }

    fn with_session<T>(
        &mut self,
        mut operation: impl FnMut(&mut Session) -> Result<T, ClientError>,
    ) -> Result<T, ClientError> {
        for attempt in 0..2 {
            if self.session.is_none() {
                self.session = Some(Session::connect(&self.endpoint)?);
            }
            let Some(session) = self.session.as_mut() else {
                continue;
            };
            match operation(session) {
                Err(ClientError::Disconnected(_) | ClientError::Io(_)) if attempt == 0 => {
                    // Reads are idempotent; one reconnect-and-retry is safe.
                    self.session = None;
                }
                other => return other,
            }
        }
        Err(ClientError::Protocol(
            "the local session could not be re-established".to_owned(),
        ))
    }
}

const fn read_only(command: &SurfaceCommand) -> bool {
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
        self.with_session(|session| match probe {
            Probe::Packages => session.packages(),
            Probe::Document(at) => session.document(at),
            Probe::Source(at) => session.source(at),
            Probe::Related(at) => session.related(at),
            Probe::Graph(at) => session.graph(at),
            Probe::Search { text, limit } => session.search_page(text, limit, continuation),
            Probe::Names { text, limit } => session.names_page(text, limit, continuation),
            Probe::Outline(path) => session.outline(path),
            Probe::OutlinePage { path, limit } => session.outline_page(path, limit, continuation),
            Probe::Index(_) | Probe::Remove(_) => Err(ClientError::Protocol(
                "the read lane never mutates the owner".to_owned(),
            )),
        })
    }

    fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
        if !read_only(&command) {
            return Err(ClientError::Protocol(
                "the read lane never runs a mutating surface command".to_owned(),
            ));
        }
        self.with_session(|session| session.surface(command.clone()))
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
}

impl<E: Engine + Send + 'static> SessionReader<E> {
    /// Wraps any engine (a fake in tests).
    #[must_use]
    pub const fn new(engine: E, loader: LocalPackageLoader) -> Self {
        Self { engine, loader }
    }
}

impl<E: Engine + Send + 'static> PageReader for SessionReader<E> {
    fn read(
        &mut self,
        request: &ReadRequest,
        context: &ReadContext<'_>,
    ) -> Result<PageValue, ReadFailure> {
        match request {
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
        }
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
        ClientError::Disconnected(_) | ClientError::Io(_) | ClientError::Transport(_) => {
            FaultCode::Transport
        }
        ClientError::BasisMismatch { .. }
        | ClientError::FreshnessMismatch
        | ClientError::RequestMismatch { .. }
        | ClientError::CursorMismatch
        | ClientError::StaleCursor => FaultCode::Cancelled,
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

fn document(engine: &mut dyn Engine, probe: Probe<'_>) -> Result<backend_library::Document, ReadFailure> {
    match engine.probe(probe).map_err(|error| failure(&error))?.reply {
        CommandReply::Document(document) | CommandReply::Page(document) => Ok(document),
        _ => Err(shape("document")),
    }
}

fn related(engine: &mut dyn Engine, symbol: &SymbolRef) -> Result<ViewSnapshot, Gap> {
    match engine.probe(Probe::Related(symbol.as_str())) {
        Ok(reply) => match reply.reply {
            CommandReply::Graph(snapshot) => Ok(snapshot),
            _ => Err(Gap::new(GapReason::ReadFailed, "the related reply changed shape")),
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
    let root = engine.revision().map_err(|error| page_mapping::client_gap(&error))?;
    if let Some(cached) = context.outlines.get(package, root) {
        return Ok(cached);
    }
    let mut rows: Vec<Row> = Vec::new();
    let mut continuation = None;
    let mut complete = false;
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
            return Err(Gap::new(GapReason::ReadFailed, "the outline page reply changed shape"));
        };
        rows.extend(page.snapshot.root.rows().iter().cloned());
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
    context.outlines.put(package.clone(), root, Arc::clone(&index));
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
        relations: snapshot.graph_relations.as_ref().map(|edges| edges.to_vec()),
        rich: snapshot.rich_graph.clone(),
    };
    let Some(centre) = hood.rows.iter().find(|row| row.label == symbol.as_str()).cloned() else {
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
                && row.signature.as_deref().is_some_and(|text| text.starts_with("nominal("))
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
        if let (Some(relations), Some(more)) = (hood.relations.as_mut(), extra.graph_relations.as_ref()) {
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
    let document = document(engine, Probe::Document(symbol.as_str()))?;
    check(context.cancel)?;
    let related = neighbourhood(engine, symbol, context);
    check(context.cancel)?;
    let references = references(engine, symbol);
    check(context.cancel)?;
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
    Ok(PageValue::Symbol(page_mapping::symbol_page(&SymbolInputs {
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
    })))
}

/// Reads a local project file for the source view. Only a local package's
/// own files are read, and only by package-relative path.
fn local_file(package: &PackageRef, path: &str) -> Option<String> {
    if !package.is_local() || path.split(['/', '\\']).any(|part| part == "..") {
        return None;
    }
    let root = Path::new(package.as_str());
    if !root.is_absolute() {
        return None;
    }
    let file = root.join(path);
    let metadata = std::fs::metadata(&file).ok()?;
    if !metadata.is_file() || metadata.len() > 4 * 1024 * 1024 {
        return None;
    }
    std::fs::read_to_string(file).ok()
}

fn compose_source(
    engine: &mut dyn Engine,
    symbol: &SymbolRef,
    context: &ReadContext<'_>,
) -> Result<PageValue, ReadFailure> {
    let document = document(engine, Probe::Source(symbol.as_str()))?;
    check(context.cancel)?;
    let package = symbol.package();
    let outline = package
        .as_ref()
        .map(|package| outline(engine, package, context));
    check(context.cancel)?;
    let references_reply = references(engine, symbol);
    let outline_index = outline.as_ref().and_then(|result| result.as_ref().ok());
    let references = page_mapping::references(
        references_reply.as_ref(),
        outline_index.map(AsRef::as_ref),
    );
    let text = match (&package, document.location.captured()) {
        (Some(package), Some(location)) => local_file(package, location.path()),
        _ => None,
    };
    Ok(PageValue::Source(page_mapping::source_view(
        symbol,
        &document,
        text.as_deref(),
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
    let outline = outline(engine, package, context);
    check(context.cancel)?;
    let local = package
        .is_local()
        .then(|| LocalProjectId::from_path(Path::new(package.as_str())).ok())
        .flatten()
        .filter(|project| project.path().is_dir())
        .map(|project| loader.load(&project));
    // Every part failing the same way means the package itself is unknown.
    if let (Err(error), None) = (&records, &local)
        && matches!(
            error,
            ClientError::Disconnected(_) | ClientError::Io(_) | ClientError::Transport(_)
        )
    {
        return Err(failure(error));
    }
    Ok(PageValue::Package(page_mapping::package_dossier(&PackageInputs {
        package,
        records: records.as_ref(),
        versions: versions.as_ref(),
        dependencies: dependencies.as_ref(),
        dependents: dependents.as_ref(),
        outline: outline.as_deref().map_err(Clone::clone),
        local: local.as_ref(),
    })))
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
    match reply.reply {
        CommandReply::Search(snapshot) => Ok(page_mapping::search_page(
            &query.text,
            &snapshot,
            context.worker,
        )),
        _ => Err(shape("search")),
    }
}

fn compose_orbit(engine: &mut dyn Engine, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
    let packages = engine.probe(Probe::Packages).and_then(|reply| match reply.reply {
        CommandReply::Packages(snapshot) => Ok(snapshot),
        _ => Err(ClientError::Protocol("the packages reply changed shape".to_owned())),
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
    use crate::model::pages::{HealthModel, IngestModel};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

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

    impl PageReader for GatedReader {
        fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
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
        Harness { pool, gate, started }
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
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut outcomes = Vec::new();
            while outcomes.len() < count {
                outcomes.extend(self.pool.drain());
                assert!(Instant::now() < deadline, "only {} outcomes arrived", outcomes.len());
                thread::sleep(Duration::from_millis(1));
            }
            outcomes
        }
    }

    fn job(name: &str, generation: u64, priority: Priority) -> ReadJob {
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
        assert!(queued_token.is_cancelled(), "the replaced queued job was cancelled");
        assert_eq!(harness.pool.queued(), 1);
        // A newer generation for the running key cancels the running job.
        harness.pool.submit(job("slow-k", 4, Priority::Normal));
        assert!(older_token.is_cancelled(), "the running job was told to stop");
        let first = harness.outcomes(1);
        assert_eq!((first[0].key.clone(), first[0].generation), (key("slow-k"), 1));
        assert_eq!(first[0].result, Err(ReadFailure::Cancelled));
        harness.release("slow-q");
        harness.release("slow-k");
        let rest = harness.outcomes(2);
        let mut ran = rest
            .iter()
            .map(|outcome| (outcome.key.to_string(), outcome.generation))
            .collect::<Vec<_>>();
        ran.sort();
        assert_eq!(ran, [("symbol slow-k".to_owned(), 4), ("symbol slow-q".to_owned(), 3)]);
    }

    #[test]
    fn normal_reads_run_before_prefetches_and_prefetches_cancel() {
        let harness = harness(1);
        harness.pool.submit(job("slow-busy", 1, Priority::Normal));
        assert_eq!(harness.started().1, "slow-busy");
        harness.pool.submit(job("fast-hover", 2, Priority::Prefetch));
        harness.pool.submit(job("fast-dropped", 3, Priority::Prefetch));
        harness.pool.submit(job("fast-click", 4, Priority::Normal));
        assert!(harness.pool.cancel(&key("fast-dropped")));
        assert_eq!(harness.pool.queued(), 2);
        harness.release("slow-busy");
        assert_eq!(harness.started().1, "fast-click");
        assert_eq!(harness.started().1, "fast-hover");
        let done = harness.outcomes(3);
        assert!(done.iter().all(|outcome| outcome.key != key("fast-dropped")));
    }

    #[test]
    fn a_promoted_prefetch_jumps_the_queue() {
        let harness = harness(1);
        harness.pool.submit(job("slow-busy", 1, Priority::Normal));
        assert_eq!(harness.started().1, "slow-busy");
        harness.pool.submit(job("fast-hover", 2, Priority::Prefetch));
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
            harness.pool.submit(job(&format!("fast-{round}"), round, Priority::Normal));
        }
        let done = harness.outcomes(8);
        assert_eq!(done.len(), 8);
        let (signals, _) = receiver.counts();
        assert_eq!(signals, 8, "one wake per finished read");
        assert!(receiver.try_take(), "the burst left one pending turn");
        assert!(!receiver.try_take(), "and only one");
    }
}
