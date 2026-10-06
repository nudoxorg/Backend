//! Bounded Cargo browse work behind the owner's existing deferred command
//! tickets. Each worker exclusively owns its cache; no mutex protects source
//! I/O or a Cargo child. The shard is chosen by the exact requested-root
//! commitment, which is present in every follow-on source request. Two
//! requested paths into one effective Cargo workspace may therefore repeat
//! an observation in separate workers; there are exactly two workers, and
//! each cache keeps its existing 128 MiB retention ceiling.
//! Each worker reserves its only reply-payload permit before execution, so
//! at most two reply payloads exist across worker locals, the completion
//! queue, and owner serialization. Admission retains at most 64 tiny owed tickets, plus at most two
//! late worker receipts after deadline terminalization.
//!
//! While open, slow source I/O cannot hold the owner loop or block health and
//! search reads. Joined close is cooperative: an uninterruptible OS file read
//! cannot be cancelled mid-syscall and may delay shutdown.

use super::super::browse::{BrowseCache, ObservationControl, with_observation_control};
use super::super::registry::RegistryGateway;
use crate::discovery::package_metadata::{FetchedPackageMetadata, PreparedPackageMetadata};
use backend_engine::{CommandFailure, CommandReply, SurfaceReply};
use backend_library::SurfaceCommand;
use std::collections::{HashMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};

const WORKERS: usize = 2;
const MAX_QUEUED_PER_WORKER: usize = 16;
const MAX_OUTSTANDING: usize = 64;

type ExecuteBrowse = dyn Fn(
        &mut BrowseCache,
        SurfaceCommand,
        Option<&backend_engine::advisory::AdvisoryAuthority>,
        &ObservationControl,
    ) -> CommandReply
    + Send
    + Sync;

struct Job {
    ticket: u64,
    request_id: u64,
    owner_cursor: backend_engine::Cursor,
    command: SurfaceCommand,
    advisory: AdvisorySelection,
    control: Arc<ObservationControl>,
    metadata: Option<PreparedPackageMetadata>,
}

struct Running {
    ticket: u64,
    command: SurfaceCommand,
    control: Arc<ObservationControl>,
}

#[derive(Default)]
struct Queue {
    pending: VecDeque<Job>,
    running: Option<Running>,
    closed: bool,
}

struct Shard {
    queue: Mutex<Queue>,
    ready: Condvar,
}

pub(super) enum Terminal {
    Reply(CommandReply, PayloadPermit),
    Metadata(FetchedPackageMetadata, PayloadPermit),
    Cancelled,
    Deadline,
    Failed,
}

/// Captures the advisory authority selected when a tree was admitted.
#[derive(Clone)]
pub(super) enum AdvisorySelection {
    NotTree,
    /// Exact registry metadata carries its own narrower authority witness.
    Metadata,
    NoAuthority,
    Authority(Arc<backend_engine::advisory::AdvisoryAuthority>),
}

impl AdvisorySelection {
    fn as_authority(&self) -> Option<&backend_engine::advisory::AdvisoryAuthority> {
        match self {
            Self::Authority(authority) => Some(authority),
            Self::NotTree | Self::Metadata | Self::NoAuthority => None,
        }
    }

    pub(super) fn still_selected(&self, registry: Option<&RegistryGateway>) -> bool {
        match self {
            Self::NotTree | Self::Metadata => true,
            Self::NoAuthority => registry.is_none(),
            Self::Authority(admitted) => registry
                .is_some_and(|registry| Arc::ptr_eq(admitted, &registry.advisory_snapshot())),
        }
    }

    pub(super) const fn is_metadata(&self) -> bool {
        matches!(self, Self::Metadata)
    }
}

struct Completion {
    ticket: u64,
    request_id: u64,
    owner_cursor: backend_engine::Cursor,
    advisory: AdvisorySelection,
    terminal: Terminal,
}

struct Outstanding {
    command: SurfaceCommand,
    control: Arc<ObservationControl>,
    request_id: u64,
    owner_cursor: backend_engine::Cursor,
    advisory: AdvisorySelection,
}

#[derive(Default)]
struct CompletionQueue {
    ready: VecDeque<Completion>,
    /// A permit is reserved before execution and held through owner encoding.
    /// Each worker owns at most one local, queued, or encoding reply payload.
    payload_reserved: [bool; WORKERS],
    closed: bool,
}

struct Completions {
    queue: Mutex<CompletionQueue>,
    capacity: Condvar,
}

/// Keeps one worker's reply slot reserved through owner serialization, even
/// after a completion has left the shared queue.
pub(super) struct PayloadPermit {
    completions: Arc<Completions>,
    shard: usize,
}

impl Drop for PayloadPermit {
    fn drop(&mut self) {
        let mut queue = self
            .completions
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        queue.payload_reserved[self.shard] = false;
        drop(queue);
        self.completions.capacity.notify_all();
    }
}

/// A fixed worker pool, owned by the command adapter and joined on close.
pub(super) struct BrowseLane {
    shards: Vec<Arc<Shard>>,
    workers: Vec<JoinHandle<()>>,
    completions: Arc<Completions>,
    outstanding: HashMap<u64, Outstanding>,
    closed: bool,
}

impl BrowseLane {
    pub(super) fn start() -> Result<Self, String> {
        Self::start_with(Arc::new(
            |cache: &mut BrowseCache,
             command: SurfaceCommand,
             advisory: Option<&backend_engine::advisory::AdvisoryAuthority>,
             _control: &ObservationControl| { execute(cache, command, advisory) },
        ))
    }

    fn start_with(executor: Arc<ExecuteBrowse>) -> Result<Self, String> {
        let completions = Arc::new(Completions {
            queue: Mutex::new(CompletionQueue::default()),
            capacity: Condvar::new(),
        });
        let mut lane = Self {
            shards: Vec::with_capacity(WORKERS),
            workers: Vec::with_capacity(WORKERS),
            completions,
            outstanding: HashMap::new(),
            closed: false,
        };
        for index in 0..WORKERS {
            let shard = Arc::new(Shard {
                queue: Mutex::new(Queue::default()),
                ready: Condvar::new(),
            });
            let worker_shard = Arc::clone(&shard);
            let worker_completions = Arc::clone(&lane.completions);
            let worker_executor = Arc::clone(&executor);
            lane.shards.push(shard);
            let worker = thread::Builder::new()
                .name(format!("locald-cargo-browse-{index}"))
                .spawn(move || run_worker(index, worker_shard, worker_completions, worker_executor))
                .map_err(|error| format!("start Cargo browse worker {index}: {error}"))?;
            lane.workers.push(worker);
        }
        Ok(lane)
    }

    pub(super) const fn accepts(command: &SurfaceCommand) -> bool {
        matches!(
            command,
            SurfaceCommand::ProjectTree { .. }
                | SurfaceCommand::CargoPackageSourceFile { .. }
                | SurfaceCommand::CargoPackageSourceInventory { .. }
                | SurfaceCommand::CargoPackageReadme { .. }
                | SurfaceCommand::CargoPackageReadmeLink { .. }
        )
    }

    /// Accepts one exact request. The caller already decoded the owner-bound
    /// command DTO. A full lane refuses admission before taking ownership.
    pub(super) fn submit(
        &mut self,
        ticket: u64,
        request_id: u64,
        owner_cursor: backend_engine::Cursor,
        command: SurfaceCommand,
        registry: Option<&RegistryGateway>,
    ) -> Result<(), &'static str> {
        self.submit_work(ticket, request_id, owner_cursor, command, registry, None)
    }

    pub(super) fn submit_metadata(
        &mut self,
        ticket: u64,
        request_id: u64,
        owner_cursor: backend_engine::Cursor,
        command: SurfaceCommand,
        metadata: PreparedPackageMetadata,
    ) -> Result<(), &'static str> {
        self.submit_work(
            ticket,
            request_id,
            owner_cursor,
            command,
            None,
            Some(metadata),
        )
    }

    fn submit_work(
        &mut self,
        ticket: u64,
        request_id: u64,
        owner_cursor: backend_engine::Cursor,
        command: SurfaceCommand,
        registry: Option<&RegistryGateway>,
        metadata: Option<PreparedPackageMetadata>,
    ) -> Result<(), &'static str> {
        if self.closed || self.outstanding.len() >= MAX_OUTSTANDING {
            return Err("Cargo browse read capacity is exhausted");
        }
        let index = shard_for(&command);
        let shard = &self.shards[index];
        let mut queue = shard.queue.lock().unwrap_or_else(PoisonError::into_inner);
        if queue.closed {
            return Err("Cargo browse read lane is closed");
        }
        if queue.pending.len() >= MAX_QUEUED_PER_WORKER
            && !queue.pending.iter().any(|job| job.command == command)
        {
            return Err("Cargo browse read queue is full");
        }
        // Include a completion already produced but not yet polled. The
        // older request must never publish after a newer exact request.
        for outstanding in self.outstanding.values() {
            if outstanding.command == command {
                outstanding.control.cancel();
            }
        }
        let mut superseded = Vec::new();
        queue.pending.retain(|job| {
            if job.command == command {
                job.control.cancel();
                superseded.push((
                    job.ticket,
                    job.request_id,
                    job.owner_cursor,
                    job.advisory.clone(),
                ));
                false
            } else {
                true
            }
        });
        if let Some(running) = &queue.running
            && running.command == command
        {
            running.control.cancel();
        }
        let control = Arc::new(ObservationControl::new());
        let advisory = if metadata.is_some() {
            AdvisorySelection::Metadata
        } else if matches!(&command, SurfaceCommand::ProjectTree { .. }) {
            registry.map_or(AdvisorySelection::NoAuthority, |registry| {
                AdvisorySelection::Authority(registry.advisory_snapshot())
            })
        } else {
            AdvisorySelection::NotTree
        };
        queue.pending.push_back(Job {
            ticket,
            request_id,
            owner_cursor,
            command: command.clone(),
            advisory: advisory.clone(),
            control: Arc::clone(&control),
            metadata,
        });
        self.outstanding.insert(
            ticket,
            Outstanding {
                command,
                control,
                request_id,
                owner_cursor,
                advisory,
            },
        );
        drop(queue);
        if !superseded.is_empty() {
            let mut completed = self
                .completions
                .queue
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            for (ticket, request_id, owner_cursor, advisory) in superseded {
                completed.ready.push_back(Completion {
                    ticket,
                    request_id,
                    owner_cursor,
                    advisory,
                    terminal: Terminal::Cancelled,
                });
            }
        }
        shard.ready.notify_one();
        Ok(())
    }

    /// Releases one response registration and cooperatively cancels only its
    /// read-only observation. A late worker completion is discarded by its
    /// ticket and releases any reserved payload permit when drained.
    pub(super) fn abandon_reply(&mut self, ticket: u64) {
        if let Some(outstanding) = self.outstanding.remove(&ticket) {
            outstanding.control.cancel();
        }
        for shard in &self.shards {
            let mut queue = shard.queue.lock().unwrap_or_else(PoisonError::into_inner);
            queue.pending.retain(|job| {
                if job.ticket == ticket {
                    job.control.cancel();
                    false
                } else {
                    true
                }
            });
            if let Some(running) = &queue.running
                && running.ticket == ticket
            {
                running.control.cancel();
            }
        }
        let mut completions = self
            .completions
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut retained = VecDeque::with_capacity(completions.ready.len());
        let mut abandoned = Vec::new();
        for completion in std::mem::take(&mut completions.ready) {
            if completion.ticket == ticket {
                abandoned.push(completion);
            } else {
                retained.push_back(completion);
            }
        }
        completions.ready = retained;
        drop(completions);
        // A completed Reply may own a payload permit whose destructor locks
        // the completion queue. Drop removed entries only after unlocking it.
        drop(abandoned);
        self.completions.capacity.notify_all();
    }

    /// Returns terminals once; a cancelled or late completion cannot publish.
    pub(super) fn drain(
        &mut self,
    ) -> Vec<(
        u64,
        u64,
        backend_engine::Cursor,
        AdvisorySelection,
        Terminal,
    )> {
        let completed = {
            let mut queue = self
                .completions
                .queue
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let completed = queue.ready.drain(..).collect::<Vec<_>>();
            drop(queue);
            completed
        };
        let mut terminals = completed
            .into_iter()
            .filter_map(|completion| {
                self.outstanding
                    .remove(&completion.ticket)
                    .map(|outstanding| {
                        let terminal = if outstanding.control.is_cancelled() {
                            Terminal::Cancelled
                        } else if outstanding.control.is_expired() {
                            Terminal::Deadline
                        } else {
                            completion.terminal
                        };
                        (
                            completion.ticket,
                            completion.request_id,
                            completion.owner_cursor,
                            completion.advisory,
                            terminal,
                        )
                    })
            })
            .collect::<Vec<_>>();
        // A worker may be in an OS read that cannot be preempted. The owner
        // still owes a terminal at the absolute deadline; late worker output
        // is discarded because its ticket is removed here.
        let expired = self
            .outstanding
            .iter()
            .filter_map(|(ticket, job)| job.control.is_expired().then_some(*ticket))
            .collect::<Vec<_>>();
        for ticket in &expired {
            if let Some(job) = self.outstanding.remove(ticket) {
                job.control.cancel();
                terminals.push((
                    *ticket,
                    job.request_id,
                    job.owner_cursor,
                    job.advisory,
                    Terminal::Deadline,
                ));
            }
        }
        if !expired.is_empty() {
            for shard in &self.shards {
                shard
                    .queue
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .pending
                    .retain(|job| !expired.contains(&job.ticket));
            }
        }
        terminals
    }

    pub(super) fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        {
            let mut completed = self
                .completions
                .queue
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            completed.closed = true;
        }
        self.completions.capacity.notify_all();
        for outstanding in self.outstanding.values() {
            outstanding.control.cancel();
        }
        for shard in &self.shards {
            let mut queue = shard.queue.lock().unwrap_or_else(PoisonError::into_inner);
            queue.closed = true;
            queue.pending.clear();
            drop(queue);
            shard.ready.notify_all();
        }
        // Cancellation stops Cargo through the platform capture boundary.
        // A source-file OS read may be uninterruptible; joining honestly waits
        // for that read rather than detaching an unbounded worker.
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
        self.outstanding.clear();
        let pending = {
            let mut queue = self
                .completions
                .queue
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            std::mem::take(&mut queue.ready)
        };
        // A reply drop releases its permit through the same queue mutex.
        drop(pending);
    }
}

impl Drop for BrowseLane {
    fn drop(&mut self) {
        self.close();
    }
}

fn shard_for(command: &SurfaceCommand) -> usize {
    use backend_library::browse::ProjectTreeRequestBindingV1;
    let requested = match command {
        SurfaceCommand::ProjectTree { root } => {
            ProjectTreeRequestBindingV1::requested_root_digest_for(Path::new(root.as_str()))
                .unwrap_or_else(|| *blake3::hash(root.as_str().as_bytes()).as_bytes())
        }
        SurfaceCommand::CargoPackageSourceFile { request, .. }
        | SurfaceCommand::CargoPackageSourceInventory { request } => {
            request.request_binding.requested_root_digest
        }
        SurfaceCommand::CargoPackageReadme { request } => request.requested_root_digest,
        SurfaceCommand::CargoPackageReadmeLink { request } => {
            request.origin.request_binding.requested_root_digest
        }
        SurfaceCommand::Package { package } => {
            *blake3::hash(package.as_str().as_bytes()).as_bytes()
        }
        _ => return 0,
    };
    usize::from(requested[0]) % WORKERS
}

fn run_worker(
    index: usize,
    shard: Arc<Shard>,
    completions: Arc<Completions>,
    executor: Arc<ExecuteBrowse>,
) {
    enum WorkReply {
        Browse(CommandReply),
        Metadata(FetchedPackageMetadata),
    }
    let mut cache = BrowseCache::default();
    loop {
        let job = {
            let mut queue = shard.queue.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                if queue.closed {
                    return;
                }
                if let Some(job) = queue.pending.pop_front() {
                    queue.running = Some(Running {
                        ticket: job.ticket,
                        command: job.command.clone(),
                        control: Arc::clone(&job.control),
                    });
                    break job;
                }
                queue = shard
                    .ready
                    .wait(queue)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        let mut reserved_payload = false;
        let terminal = if job.control.is_cancelled() {
            Terminal::Cancelled
        } else if job.control.is_expired() {
            Terminal::Deadline
        } else {
            // Reserve the worker's only payload permit before the executor
            // can allocate a reply. Another shard has its own permit.
            let mut completed = completions
                .queue
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            while !completed.closed && completed.payload_reserved[index] {
                completed = completions
                    .capacity
                    .wait(completed)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            if completed.closed {
                return;
            }
            completed.payload_reserved[index] = true;
            reserved_payload = true;
            drop(completed);
            if job.control.is_cancelled() {
                Terminal::Cancelled
            } else if job.control.is_expired() {
                Terminal::Deadline
            } else {
                let reply =
                    catch_unwind(AssertUnwindSafe(|| {
                        with_observation_control(Arc::clone(&job.control), || {
                            if let Some(metadata) = job.metadata {
                                WorkReply::Metadata(metadata.fetch(|| {
                                    job.control.is_cancelled() || job.control.is_expired()
                                }))
                            } else {
                                WorkReply::Browse(executor(
                                    &mut cache,
                                    job.command,
                                    job.advisory.as_authority(),
                                    &job.control,
                                ))
                            }
                        })
                    }));
                if job.control.is_cancelled() {
                    Terminal::Cancelled
                } else if job.control.is_expired() {
                    Terminal::Deadline
                } else {
                    match reply {
                        Ok(WorkReply::Browse(reply)) => Terminal::Reply(
                            reply,
                            PayloadPermit {
                                completions: Arc::clone(&completions),
                                shard: index,
                            },
                        ),
                        Ok(WorkReply::Metadata(metadata)) => Terminal::Metadata(
                            metadata,
                            PayloadPermit {
                                completions: Arc::clone(&completions),
                                shard: index,
                            },
                        ),
                        Err(_) => {
                            // An unexpected cache panic cannot strand an owed
                            // transport ticket or retain partially changed state.
                            cache = BrowseCache::default();
                            Terminal::Failed
                        }
                    }
                }
            }
        };
        shard
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .running = None;
        let payload = matches!(&terminal, Terminal::Reply(_, _) | Terminal::Metadata(_, _));
        let mut completed = completions
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if completed.closed {
            return;
        }
        if reserved_payload && !payload {
            completed.payload_reserved[index] = false;
            completions.capacity.notify_all();
        }
        completed.ready.push_back(Completion {
            ticket: job.ticket,
            request_id: job.request_id,
            owner_cursor: job.owner_cursor,
            advisory: job.advisory,
            terminal,
        });
    }
}

fn execute(
    cache: &mut BrowseCache,
    command: SurfaceCommand,
    advisory: Option<&backend_engine::advisory::AdvisoryAuthority>,
) -> CommandReply {
    match command {
        SurfaceCommand::ProjectTree { root } => cache
            .project_tree(Path::new(root.as_str()), advisory)
            .map_or_else(
                |error| CommandReply::Failed(CommandFailure::InvalidQuery(error)),
                |tree| CommandReply::Surface(SurfaceReply::ProjectTree(Box::new(tree))),
            ),
        SurfaceCommand::CargoPackageSourceFile { request, path } => CommandReply::Surface(
            SurfaceReply::CargoPackageSourceFile(cache.source_file(request, path)),
        ),
        SurfaceCommand::CargoPackageSourceInventory { request } => CommandReply::Surface(
            SurfaceReply::CargoPackageSourceInventory(cache.source_inventory(request)),
        ),
        SurfaceCommand::CargoPackageReadme { request } => CommandReply::Surface(
            SurfaceReply::CargoPackageReadme(cache.package_readme(request)),
        ),
        SurfaceCommand::CargoPackageReadmeLink { request } => CommandReply::Surface(
            SurfaceReply::CargoPackageReadmeLink(cache.package_readme_link(request)),
        ),
        _ => CommandReply::Failed(CommandFailure::InvalidQuery(
            "the Cargo browse lane received another command".to_owned(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_library::ProductText;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn tree(path: &str) -> SurfaceCommand {
        SurfaceCommand::ProjectTree {
            root: ProductText::new(path).expect("bounded test path"),
        }
    }

    fn other_shard(command: &SurfaceCommand) -> SurfaceCommand {
        (0..100)
            .map(|index| tree(&format!("/tmp/browse-lane-other-{index}")))
            .find(|candidate| shard_for(candidate) != shard_for(command))
            .expect("a different request-root shard")
    }

    fn same_shard(command: &SurfaceCommand) -> SurfaceCommand {
        (0..100)
            .map(|index| tree(&format!("/tmp/browse-lane-same-{index}")))
            .find(|candidate| candidate != command && shard_for(candidate) == shard_for(command))
            .expect("another request in the same shard")
    }

    fn wait_for(
        lane: &mut BrowseLane,
        ticket: u64,
        timeout: Duration,
    ) -> (
        u64,
        u64,
        backend_engine::Cursor,
        AdvisorySelection,
        Terminal,
    ) {
        let deadline = Instant::now() + timeout;
        loop {
            for completion in lane.drain() {
                if completion.0 == ticket {
                    return completion;
                }
            }
            assert!(
                Instant::now() < deadline,
                "browse ticket {ticket} did not finish"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn blocked_request_does_not_block_another_shard_and_old_result_cannot_land() {
        let slow = tree("/tmp/browse-lane-blocked");
        let fast = other_shard(&slow);
        let entered = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let executor = {
            let slow = slow.clone();
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            Arc::new(
                move |_: &mut BrowseCache,
                      command: SurfaceCommand,
                      _: Option<&backend_engine::advisory::AdvisoryAuthority>,
                      _: &ObservationControl| {
                    if command == slow {
                        entered.store(true, Ordering::Release);
                        while !release.load(Ordering::Acquire) {
                            thread::sleep(Duration::from_millis(5));
                        }
                    }
                    CommandReply::Failed(CommandFailure::InvalidQuery("done".to_owned()))
                },
            ) as Arc<ExecuteBrowse>
        };
        let mut lane = BrowseLane::start_with(executor).expect("browse lane");
        assert_eq!(lane.workers.len(), WORKERS);
        lane.submit(1, 11, backend_engine::Cursor::new(), slow.clone(), None)
            .expect("first request");
        let start = Instant::now();
        while !entered.load(Ordering::Acquire) {
            assert!(
                start.elapsed() < Duration::from_secs(1),
                "worker did not start"
            );
            thread::sleep(Duration::from_millis(5));
        }
        lane.submit(2, 22, backend_engine::Cursor::new(), fast, None)
            .expect("independent request");
        let (_, _, _, _, fast_terminal) = wait_for(&mut lane, 2, Duration::from_secs(1));
        assert!(matches!(fast_terminal, Terminal::Reply(_, _)));
        lane.submit(3, 33, backend_engine::Cursor::new(), slow, None)
            .expect("newest request");
        release.store(true, Ordering::Release);
        let mut old = None;
        let mut new = None;
        let deadline = Instant::now() + Duration::from_secs(1);
        while old.is_none() || new.is_none() {
            for (ticket, _, _, _, terminal) in lane.drain() {
                match ticket {
                    1 => old = Some(terminal),
                    3 => new = Some(terminal),
                    _ => {}
                }
            }
            assert!(
                Instant::now() < deadline,
                "superseded requests did not finish"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(old, Some(Terminal::Cancelled)));
        assert!(matches!(new, Some(Terminal::Reply(_, _))));
        lane.close();
        assert!(lane.workers.is_empty());
    }

    #[test]
    fn reply_payload_permit_is_reserved_before_execution_per_shard() {
        use std::sync::atomic::AtomicUsize;
        let first = tree("/tmp/browse-lane-payload-first");
        let second = same_shard(&first);
        let independent = other_shard(&first);
        let same_started = Arc::new(AtomicUsize::new(0));
        let independent_started = Arc::new(AtomicUsize::new(0));
        let executor = {
            let same_started = Arc::clone(&same_started);
            let independent_started = Arc::clone(&independent_started);
            let independent = independent.clone();
            Arc::new(
                move |_: &mut BrowseCache,
                      command: SurfaceCommand,
                      _: Option<&backend_engine::advisory::AdvisoryAuthority>,
                      _: &ObservationControl| {
                    if command == independent {
                        independent_started.fetch_add(1, Ordering::AcqRel);
                    } else {
                        same_started.fetch_add(1, Ordering::AcqRel);
                    }
                    CommandReply::Failed(CommandFailure::InvalidQuery("payload".to_owned()))
                },
            ) as Arc<ExecuteBrowse>
        };
        let mut lane = BrowseLane::start_with(executor).expect("browse lane");
        for (ticket, command) in [(1, first), (2, second), (3, independent)] {
            lane.submit(ticket, ticket, backend_engine::Cursor::new(), command, None)
                .expect("bounded request");
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let queued = lane
                .completions
                .queue
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if queued.ready.len() == 2 {
                assert!(queued.payload_reserved.iter().all(|reserved| *reserved));
                break;
            }
            drop(queued);
            assert!(
                Instant::now() < deadline,
                "independent reply did not complete"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(same_started.load(Ordering::Acquire), 1);
        assert_eq!(independent_started.load(Ordering::Acquire), 1);
        let completions = lane.drain();
        assert_eq!(completions.len(), 2);
        // Owner may be busy binding and encoding these moved replies. The
        // drained objects still own both permits; no third payload may start.
        thread::sleep(Duration::from_millis(50));
        assert_eq!(same_started.load(Ordering::Acquire), 1);
        assert!(
            lane.completions
                .queue
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .payload_reserved
                .iter()
                .all(|reserved| *reserved)
        );
        drop(completions);
        let deadline = Instant::now() + Duration::from_secs(1);
        while same_started.load(Ordering::Acquire) != 2 {
            assert!(
                Instant::now() < deadline,
                "second same-shard job did not start"
            );
            thread::sleep(Duration::from_millis(5));
        }
        let (_, _, _, _, terminal) = wait_for(&mut lane, 2, Duration::from_secs(1));
        assert!(matches!(terminal, Terminal::Reply(_, _)));
        lane.close();
    }

    #[test]
    fn abandoning_queued_reply_drops_its_payload_permit_after_unlocking() {
        let mut lane = BrowseLane::start().expect("browse lane");
        {
            let mut queue = lane
                .completions
                .queue
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            queue.payload_reserved[0] = true;
            queue.ready.push_back(Completion {
                ticket: 77,
                request_id: 700,
                owner_cursor: backend_engine::Cursor::new(),
                advisory: AdvisorySelection::NotTree,
                terminal: Terminal::Reply(
                    CommandReply::Failed(CommandFailure::InvalidQuery("ready".to_owned())),
                    PayloadPermit {
                        completions: Arc::clone(&lane.completions),
                        shard: 0,
                    },
                ),
            });
        }

        let (released_tx, released_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            lane.abandon_reply(77);
            let released = !lane
                .completions
                .queue
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .payload_reserved[0];
            let _ = released_tx.send(released);
            lane.close();
        });
        assert_eq!(
            released_rx.recv_timeout(Duration::from_secs(1)),
            Ok(true),
            "abandon must not drop a permit while holding its queue lock"
        );
        worker.join().expect("abandon worker exits");
    }

    #[test]
    fn shutdown_cancels_and_joins_the_fixed_workers() {
        let entered = Arc::new(AtomicBool::new(false));
        let executor = {
            let entered = Arc::clone(&entered);
            Arc::new(
                move |_: &mut BrowseCache,
                      _: SurfaceCommand,
                      _: Option<&backend_engine::advisory::AdvisoryAuthority>,
                      control: &ObservationControl| {
                    entered.store(true, Ordering::Release);
                    while !control.is_cancelled() {
                        thread::sleep(Duration::from_millis(5));
                    }
                    CommandReply::Failed(CommandFailure::InvalidQuery("cancelled".to_owned()))
                },
            ) as Arc<ExecuteBrowse>
        };
        let mut lane = BrowseLane::start_with(executor).expect("browse lane");
        lane.submit(
            1,
            11,
            backend_engine::Cursor::new(),
            tree("/tmp/browse-lane-shutdown"),
            None,
        )
        .expect("request");
        let start = Instant::now();
        while !entered.load(Ordering::Acquire) {
            assert!(
                start.elapsed() < Duration::from_secs(1),
                "worker did not start"
            );
            thread::sleep(Duration::from_millis(5));
        }
        lane.close();
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "shutdown did not join"
        );
        assert!(lane.workers.is_empty());
        assert!(lane.outstanding.is_empty());
    }

    #[test]
    fn queued_requests_have_a_fixed_per_shard_limit() {
        let blocked = tree("/tmp/browse-lane-capacity");
        let shard = shard_for(&blocked);
        let entered = Arc::new(AtomicBool::new(false));
        let executor = {
            let entered = Arc::clone(&entered);
            Arc::new(
                move |_: &mut BrowseCache,
                      _: SurfaceCommand,
                      _: Option<&backend_engine::advisory::AdvisoryAuthority>,
                      control: &ObservationControl| {
                    entered.store(true, Ordering::Release);
                    while !control.is_cancelled() {
                        thread::sleep(Duration::from_millis(5));
                    }
                    CommandReply::Failed(CommandFailure::InvalidQuery("cancelled".to_owned()))
                },
            ) as Arc<ExecuteBrowse>
        };
        let mut lane = BrowseLane::start_with(executor).expect("browse lane");
        lane.submit(1, 1, backend_engine::Cursor::new(), blocked, None)
            .expect("running request");
        let deadline = Instant::now() + Duration::from_secs(1);
        while !entered.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "worker did not start");
            thread::sleep(Duration::from_millis(5));
        }
        let commands = (0..1000)
            .map(|index| tree(&format!("/tmp/browse-lane-capacity-{index}")))
            .filter(|command| shard_for(command) == shard)
            .take(MAX_QUEUED_PER_WORKER + 1)
            .collect::<Vec<_>>();
        assert_eq!(commands.len(), MAX_QUEUED_PER_WORKER + 1);
        for (index, command) in commands.iter().take(MAX_QUEUED_PER_WORKER).enumerate() {
            lane.submit(
                index as u64 + 2,
                index as u64 + 2,
                backend_engine::Cursor::new(),
                command.clone(),
                None,
            )
            .expect("bounded queued request");
        }
        assert_eq!(
            lane.submit(
                99,
                99,
                backend_engine::Cursor::new(),
                commands[MAX_QUEUED_PER_WORKER].clone(),
                None,
            ),
            Err("Cargo browse read queue is full")
        );
        assert_eq!(lane.outstanding.len(), MAX_QUEUED_PER_WORKER + 1);
        lane.close();
        assert!(lane.workers.is_empty());
    }

    #[test]
    fn deadline_delivers_one_terminal_even_if_worker_finishes_later() {
        let entered = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let executor = {
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            Arc::new(
                move |_: &mut BrowseCache,
                      _: SurfaceCommand,
                      _: Option<&backend_engine::advisory::AdvisoryAuthority>,
                      _: &ObservationControl| {
                    entered.store(true, Ordering::Release);
                    while !release.load(Ordering::Acquire) {
                        thread::sleep(Duration::from_millis(5));
                    }
                    CommandReply::Failed(CommandFailure::InvalidQuery("late".to_owned()))
                },
            ) as Arc<ExecuteBrowse>
        };
        let mut lane = BrowseLane::start_with(executor).expect("browse lane");
        lane.submit(
            1,
            11,
            backend_engine::Cursor::new(),
            tree("/tmp/browse-lane-deadline"),
            None,
        )
        .expect("request");
        let start = Instant::now();
        while !entered.load(Ordering::Acquire) {
            assert!(
                start.elapsed() < Duration::from_secs(1),
                "worker did not start"
            );
            thread::sleep(Duration::from_millis(5));
        }
        lane.outstanding
            .get(&1)
            .expect("active request")
            .control
            .expire_for_test();
        let ready = lane.drain();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].0, 1);
        assert!(matches!(ready[0].4, Terminal::Deadline));
        release.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(1);
        while lane
            .completions
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .ready
            .is_empty()
        {
            assert!(Instant::now() < deadline, "worker did not retire");
            thread::sleep(Duration::from_millis(5));
        }
        assert!(lane.drain().is_empty(), "late result must be discarded");
        lane.close();
    }
}
