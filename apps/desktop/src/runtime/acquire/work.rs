//! The worker that adds releases to the library: one thread, one job at a
//! time, in order, each release indexed by the owner with `Session::index`.
//!
//! A job is a release a person asked for (Find, a package page), which goes
//! to the front, or the registry packages a project builds with, which go to
//! the back. The functions here take a session and a source and nothing of
//! the window, so a test drives them against a real owner exactly as the
//! worker does.

use super::{OwnerRequest, Stage};
use super::super::actor::{CancellationToken, CancellationWake};
use backend_client::{RequestContext, RequestCancellationReason};
use crate::core::LocalProjectId;
use crate::host::registry::{Availability, Composition, CompositionGeneration, RegistrySource};
use crate::model::pages::PackageRef;
use crate::model::release::Release;
use backend_client::Session;
use backend_library::browse::{PackageOrigin, PackageRole, ProjectTree};
use backend_library::{ProductText, SurfaceCommand, SurfaceReply};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

/// One queued job's caller lifetime. The cancellation registration is made
/// before it waits in the queue, not against whichever owner drains it later.
struct WorkOwner {
    owner: OwnerRequest,
    generation: CompositionGeneration,
    #[cfg(test)]
    producer: Option<Arc<std::sync::RwLock<Option<crate::host::registry::ServingComposition>>>>,
    cancel: CancellationToken,
    context: RequestContext,
    _owner_wake: Option<CancellationWake>,
    _transport_wake: CancellationWake,
}

impl WorkOwner {
    fn new(owner: OwnerRequest, generation: CompositionGeneration, #[cfg(test)] producer: Option<Arc<std::sync::RwLock<Option<crate::host::registry::ServingComposition>>>>) -> Self {
        let cancel = CancellationToken::new();
        let context = RequestContext::new();
        let interrupted = context.clone();
        let transport_wake = cancel.on_cancel(move || interrupted.cancel(RequestCancellationReason::Shutdown));
        let owner_wake = owner.lease.bind_cancellation(&cancel);
        Self { owner, generation, #[cfg(test)] producer, cancel, context, _owner_wake: owner_wake, _transport_wake: transport_wake }
    }

    fn current(&self) -> bool {
        if self.cancel.is_cancelled() || !self.owner.current() { return false; }
        #[cfg(test)]
        if let Some(producer) = &self.producer {
            return crate::host::registry::test_registry_in(producer).is_some_and(|composition| composition.generation == self.generation);
        }
        crate::host::registry::composed().is_some_and(|composition| composition.generation == self.generation)
    }

    fn check(&self) -> Result<(), backend_client::ClientError> {
        self.context.check()?;
        if !self.current() { return Err(backend_client::ClientError::Protocol("The acquisition's index owner was replaced. Nothing further was sent.".into())); }
        Ok(())
    }

    fn connect(&self, endpoint: &Path) -> Result<Session, backend_client::ClientError> {
        self.check()?;
        let mut session = Session::connect_with_context(endpoint, self.context.clone())?;
        // Read-only discovery must not adopt a new producer at a reused path
        // either. First mutations repeat this check after intervening reads.
        self.before_send(&mut session)?;
        Ok(session)
    }

    /// Same producer-stream check as project indexing, immediately before
    /// each first mutation send. Cancellation only abandons a reply wait; it
    /// cannot undo a command the owner has already accepted.
    fn before_send(&self, session: &mut Session) -> Result<(), backend_client::ClientError> {
        self.check()?;
        let certified = self.owner.lease.ready_for_send(self.owner.basis, &self.cancel)
            .map_err(|words| backend_client::ClientError::Protocol(words.to_string()))?;
        if self.owner.lease.is_gated() {
            let revision = session.revision()?;
            let socket = crate::core::VersionedRoot::from_revision(certified.producer_epoch(), revision.cursor(), 0);
            if !super::super::actor::admits_mutation_root(certified, socket) {
                return Err(backend_client::ClientError::Protocol("The acquisition socket does not match its captured index producer. Nothing was sent.".into()));
            }
        }
        self.owner.lease.ready_for_send(self.owner.basis, &self.cancel)
            .map_err(|words| backend_client::ClientError::Protocol(words.to_string()))?;
        self.check()
    }
}

impl Drop for WorkOwner {
    fn drop(&mut self) { self.context.finish(); }
}

fn connect(composition: &Composition, owner: Option<&WorkOwner>) -> Result<Session, backend_client::ClientError> {
    match owner { Some(owner) => owner.connect(&composition.endpoint), None => Session::connect(&composition.endpoint) }
}

fn check(owner: Option<&WorkOwner>) -> Result<(), backend_client::ClientError> {
    owner.map_or(Ok(()), WorkOwner::check)
}

fn before_send(owner: Option<&WorkOwner>, session: &mut Session) -> Result<(), backend_client::ClientError> {
    owner.map_or(Ok(()), |owner| owner.before_send(session))
}

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
    jobs: VecDeque<Queued>,
    running: bool,
}

/// The producer and destination belong to this job, not to the thread that
/// happened to begin draining the queue.
struct Queued {
    job: Job,
    composition: Composition,
    owner: WorkOwner,
    post: Arc<dyn Fn(Landed) + Send + Sync>,
    #[cfg(test)]
    producer: Option<Arc<std::sync::RwLock<Option<crate::host::registry::ServingComposition>>>>,
}

impl Queued {
    fn serving_generation(&self) -> Option<CompositionGeneration> {
        #[cfg(test)]
        if let Some(slot) = &self.producer {
            return crate::host::registry::test_registry_in(slot).map(|composition| composition.generation);
        }
        crate::host::registry::composed().map(|composition| composition.generation)
    }
    fn current(&self, generation: Option<CompositionGeneration>) -> bool {
        generation == Some(self.composition.generation) && self.owner.current()
    }
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
    pub(crate) fn push(self: &Arc<Self>, job: Job, composition: Composition, owner: OwnerRequest, post: Arc<dyn Fn(Landed) + Send + Sync>) -> bool {
        #[cfg(test)]
        return self.push_in(job, composition, owner, post, None);
        #[cfg(not(test))]
        self.push_in(job, composition, owner, post)
    }

    #[cfg(test)]
    pub(super) fn push_fixture(self: &Arc<Self>, job: Job, composition: Composition, post: Arc<dyn Fn(Landed) + Send + Sync>, producer: Arc<std::sync::RwLock<Option<crate::host::registry::ServingComposition>>>) -> bool {
        self.push_owned_fixture(job, composition, OwnerRequest::fixture(), post, producer)
    }

    #[cfg(test)]
    pub(super) fn push_owned_fixture(self: &Arc<Self>, job: Job, composition: Composition, owner: OwnerRequest, post: Arc<dyn Fn(Landed) + Send + Sync>, producer: Arc<std::sync::RwLock<Option<crate::host::registry::ServingComposition>>>) -> bool {
        self.push_in(job, composition, owner, post, Some(producer))
    }

    fn push_in(self: &Arc<Self>, job: Job, composition: Composition, owner: OwnerRequest, post: Arc<dyn Fn(Landed) + Send + Sync>, #[cfg(test)] producer: Option<Arc<std::sync::RwLock<Option<crate::host::registry::ServingComposition>>>>) -> bool {
        let start = {
            let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
            if pending.jobs.iter().any(|queued| queued.job == job && queued.composition.generation == composition.generation && queued.owner.owner.same_owner(&owner) && queued.owner.current()) {
                return false;
            }
            let release = matches!(&job, Job::Release(_));
            let queued = Queued { job, owner: WorkOwner::new(owner, composition.generation, #[cfg(test)] producer.clone()), composition, post, #[cfg(test)] producer };
            if release {
                pending.jobs.push_front(queued);
            } else {
                pending.jobs.push_back(queued);
            }
            !std::mem::replace(&mut pending.running, true)
        };
        if start {
            let queue = Arc::clone(self);
            let spawned = std::thread::Builder::new().name("nudox-acquire".to_owned()).spawn(move || queue.drain());
            if spawned.is_err() {
                self.pending.lock().unwrap_or_else(PoisonError::into_inner).running = false;
            }
        }
        true
    }

    /// Whether a worker is draining the queue.
    #[cfg(feature = "visual-harness")]
    pub(crate) fn running(&self) -> bool {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner).running
    }

    fn next(&self) -> Option<Queued> {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        let job = pending.jobs.pop_front();
        if job.is_none() {
            pending.running = false;
        }
        job
    }

    fn drain(&self) {
        while let Some(queued) = self.next() {
            if !queued.current(queued.serving_generation()) { continue; }
            let Queued { job, composition, owner, post, .. } = queued;
            // A panic is that job's failure, never a worker that silently stops.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_in(&job, &composition, Some(&owner), &|landed| { if owner.current() { post(landed); } })));
            if let Err(panic) = outcome && owner.current() {
                let words: Arc<str> = Arc::from(format!("the work panicked: {}", super::super::offload::describe(panic.as_ref())));
                match job {
                    Job::Release(release) => post(Landed::Stage(release, Stage::Failed(words))),
                    Job::Dependencies(project) => post(Landed::Dependencies(project, Err(words))),
                }
            }
        }
    }
}

#[cfg(test)]
mod queue_tests {
    use super::*;
    use crate::host::registry::{CrateName, Published, SourceError, SourceTree};
    use std::path::PathBuf;

    struct RefusingSource(&'static str);
    impl RegistrySource for RefusingSource {
        fn releases(&self, _: &CrateName) -> Vec<Published> { vec![] }
        fn offline(&self, _: &str, _: usize) -> Vec<Release> { vec![] }
        fn availability(&self, _: &Release) -> Availability {
            Availability::Ambiguous { indexes: vec![PathBuf::from(self.0)] }
        }
        fn release_of(&self, _: &Path) -> Option<Release> { None }
        fn resolve(&self, release: &Release) -> Result<SourceTree, SourceError> { Err(SourceError::NeedsDownload(release.clone())) }
    }
    fn composition(generation: u64, source: &'static str) -> Composition {
        Composition { endpoint: PathBuf::from(format!("/owner/{generation}")), source: Arc::new(RefusingSource(source)),
            authority: source.into(), generation: CompositionGeneration(generation), refusals: None }
    }
    fn held_queue() -> Arc<Queue> {
        let queue = Queue::new();
        queue.pending.lock().expect("queue").running = true;
        queue
    }

    struct CountedRefusal { calls: Arc<std::sync::atomic::AtomicUsize> }
    impl RegistrySource for CountedRefusal {
        fn releases(&self, _: &CrateName) -> Vec<Published> { vec![] }
        fn offline(&self, _: &str, _: usize) -> Vec<Release> { vec![] }
        fn availability(&self, _: &Release) -> Availability {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Availability::Ambiguous { indexes: vec![PathBuf::from("counted-source")] }
        }
        fn release_of(&self, _: &Path) -> Option<Release> { None }
        fn resolve(&self, release: &Release) -> Result<SourceTree, SourceError> { Err(SourceError::NeedsDownload(release.clone())) }
    }

    fn owned(gate: &super::super::super::owner::OwnerGate, basis: crate::core::VersionedRoot) -> OwnerRequest {
        OwnerRequest::new(super::super::super::actor::IndexMutationLease::capture(Some(gate), gate.ready_epoch()).expect("current mutation lease"), basis)
    }

    #[test]
    fn the_real_drain_retires_same_composition_index_replacements_in_both_modes() {
        use super::super::super::owner::{OwnerGate, OwnerState};
        use std::sync::atomic::{AtomicUsize, Ordering};
        for mode in [crate::model::ServiceMode::Embedded, crate::model::ServiceMode::Attached] {
            let basis = crate::core::VersionedRoot::synthetic(backend_library::view_state_root(&[]), 4);
            let gate = OwnerGate::ready(basis, mode);
            let queue = held_queue();
            let producer = Arc::new(std::sync::RwLock::new(None));
            let calls = Arc::new(AtomicUsize::new(0));
            let composition = crate::host::registry::test_install_registry(&producer,
                Composition { source: Arc::new(CountedRefusal { calls: Arc::clone(&calls) }), ..composition(0, "same-source") });
            let job = Job::Release(Release::new("same", "0.1.0").expect("release"));
            let old_posts = Arc::new(Mutex::new(Vec::new()));
            let old_sink = Arc::clone(&old_posts);
            assert!(queue.push_owned_fixture(job.clone(), composition.clone(), owned(&gate, basis),
                Arc::new(move |landed| old_sink.lock().expect("old posts").push(landed)), Arc::clone(&producer)));
            gate.publish(OwnerState::Starting);
            gate.publish(OwnerState::Ready { key: basis, mode });
            let fresh_posts = Arc::new(Mutex::new(Vec::new()));
            let fresh_sink = Arc::clone(&fresh_posts);
            assert!(queue.push_owned_fixture(job, composition, owned(&gate, basis),
                Arc::new(move |landed| fresh_sink.lock().expect("current posts").push(landed)), producer),
                "same composition and release must not deduplicate against a replaced index owner");
            queue.drain();
            assert!(old_posts.lock().expect("old posts").is_empty());
            assert_eq!(calls.load(Ordering::SeqCst), 1, "only the currently captured owner can resolve source");
            assert!(matches!(fresh_posts.lock().expect("current posts").last(), Some(Landed::Stage(_, Stage::Failed(_)))));
            assert!(!queue.pending.lock().expect("queue").running);
        }
    }

    struct ReplacingSource {
        gate: super::super::super::owner::OwnerGate,
        basis: crate::core::VersionedRoot,
        mode: crate::model::ServiceMode,
        during_resolve: bool,
        root: PathBuf,
        resolved: std::sync::atomic::AtomicUsize,
    }
    impl ReplacingSource {
        fn replace(&self) {
            self.gate.publish(super::super::super::owner::OwnerState::Starting);
            self.gate.publish(super::super::super::owner::OwnerState::Ready { key: self.basis, mode: self.mode });
        }
    }
    impl RegistrySource for ReplacingSource {
        fn releases(&self, _: &CrateName) -> Vec<Published> { vec![] }
        fn offline(&self, _: &str, _: usize) -> Vec<Release> { vec![] }
        fn availability(&self, _: &Release) -> Availability {
            if !self.during_resolve { self.replace(); }
            Availability::Unpacked(self.root.clone())
        }
        fn release_of(&self, _: &Path) -> Option<Release> { None }
        fn resolve(&self, release: &Release) -> Result<SourceTree, SourceError> {
            self.resolved.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.during_resolve { self.replace(); }
            Ok(SourceTree { release: release.clone(), root: self.root.clone(), origin: crate::host::registry::Origin::Cargo })
        }
    }

    #[cfg(unix)]
    #[test]
    fn source_work_cannot_reconnect_or_write_refusals_after_its_captured_owner_retires() {
        use std::os::unix::net::UnixListener;
        use std::sync::atomic::{AtomicUsize, Ordering};
        static CASES: AtomicUsize = AtomicUsize::new(0);
        for mode in [crate::model::ServiceMode::Embedded, crate::model::ServiceMode::Attached] {
            for during_resolve in [false, true] {
                let directory = std::env::temp_dir().join(format!("nacq-{}-{}", std::process::id(), CASES.fetch_add(1, Ordering::Relaxed)));
                std::fs::create_dir(&directory).expect("own fixture directory");
                let endpoint = directory.join("owner.sock");
                let listener = UnixListener::bind(&endpoint).expect("replacement endpoint");
                listener.set_nonblocking(true).expect("nonblocking admission witness");
                let refusal = directory.join("refusals.json");
                std::fs::write(&refusal, b"{}\n").expect("retained refusal bytes");
                let basis = crate::core::VersionedRoot::synthetic(backend_library::view_state_root(&[]), 4);
                let gate = super::super::super::owner::OwnerGate::ready(basis, mode);
                let source = Arc::new(ReplacingSource { gate: gate.clone(), basis, mode, during_resolve, root: directory.clone(), resolved: AtomicUsize::new(0) });
                let producer = Arc::new(std::sync::RwLock::new(None));
                let composition = crate::host::registry::test_install_registry(&producer, Composition {
                    endpoint, source: source.clone(), authority: "same-source".into(), generation: CompositionGeneration(0), refusals: Some(refusal.clone()) });
                let owner = WorkOwner::new(owned(&gate, basis), composition.generation, Some(producer));
                let context = owner.context.clone();
                let release = Release::new("retired", "0.1.0").expect("release");
                let stage = index_release_in(&composition, &release, Some(&owner), Listed::Ready, &|_| {});
                assert!(matches!(stage, Stage::Failed(_)));
                assert_eq!(source.resolved.load(Ordering::SeqCst), usize::from(during_resolve));
                assert_eq!(context.cancellation_reason(), Some(RequestCancellationReason::Shutdown));
                assert!(matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
                    "source work returning after replacement cannot connect to the new same-endpoint owner");
                assert_eq!(std::fs::read(&refusal).expect("refusal bytes"), b"{}\n");
                drop(listener);
                std::fs::remove_dir_all(&directory).expect("own fixture cleanup");
            }
        }
    }

    struct CountedTransport(Arc<std::sync::atomic::AtomicUsize>);
    impl backend_client::CommandTransport for CountedTransport {
        fn request(&mut self, _: backend_library::CommandDto) -> Result<backend_library::ReplyDto, backend_client::ClientError> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(backend_client::ClientError::Protocol("counted socket witness".into()))
        }
    }

    #[cfg(unix)]
    #[test]
    fn captured_owner_cancellation_interrupts_only_its_bound_socket() {
        use std::io::Read;
        use std::os::unix::net::UnixStream;
        for mode in [crate::model::ServiceMode::Embedded, crate::model::ServiceMode::Attached] {
            let basis = crate::core::VersionedRoot::synthetic(backend_library::view_state_root(&[]), 4);
            let gate = super::super::super::owner::OwnerGate::ready(basis, mode);
            let producer = Arc::new(std::sync::RwLock::new(None));
            let composition = crate::host::registry::test_install_registry(&producer, composition(0, "same-source"));
            let old = WorkOwner::new(owned(&gate, basis), composition.generation, Some(Arc::clone(&producer)));
            let (client, mut peer) = UnixStream::pair().expect("old exact socket pair");
            peer.set_read_timeout(Some(std::time::Duration::from_secs(1))).expect("bounded old socket witness");
            let mut session = Session::from_transport(&composition.endpoint, backend_client::UnixCommandTransport::from_stream(client));
            session.set_request_context(Some(old.context.clone())).expect("bind exact old socket");
            gate.publish(super::super::super::owner::OwnerState::Starting);
            gate.publish(super::super::super::owner::OwnerState::Ready { key: basis, mode });
            assert_eq!(old.context.cancellation_reason(), Some(RequestCancellationReason::Shutdown));
            let mut byte = [0];
            assert_eq!(peer.read(&mut byte).expect("interrupted peer closes"), 0);
            let current = WorkOwner::new(owned(&gate, basis), composition.generation, Some(producer));
            let (client, mut peer) = UnixStream::pair().expect("new exact socket pair");
            peer.set_read_timeout(Some(std::time::Duration::from_millis(50))).expect("bounded current socket witness");
            let mut next = Session::from_transport(&composition.endpoint, backend_client::UnixCommandTransport::from_stream(client));
            next.set_request_context(Some(current.context.clone())).expect("bind exact new socket");
            old.cancel.cancel();
            drop(old);
            assert!(current.context.check().is_ok());
            assert!(matches!(peer.read(&mut byte), Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)),
                "retiring the old job must leave the new owner's socket open");
        }
    }

    #[test]
    fn dependency_jobs_retain_the_original_project_root_and_mutation_basis() {
        let basis = crate::core::VersionedRoot::synthetic(backend_library::view_state_root(&[]), 4);
        let gate = super::super::super::owner::OwnerGate::ready(basis, crate::model::ServiceMode::Attached);
        let queue = held_queue();
        let producer = Arc::new(std::sync::RwLock::new(None));
        let composition = crate::host::registry::test_install_registry(&producer, composition(0, "same-source"));
        let first = LocalProjectId::new("/fixture/first-indexed-root").expect("first root");
        let second = LocalProjectId::new("/fixture/second-indexed-root").expect("second root");
        let captured = owned(&gate, basis);
        for project in [&first, &second] {
            assert!(queue.push_owned_fixture(Job::Dependencies(project.clone()), composition.clone(), captured.clone(), Arc::new(|_| {}), Arc::clone(&producer)));
        }
        for project in [first, second] {
            let queued = queue.next().expect("exact dependent root job");
            assert_eq!(queued.job, Job::Dependencies(project));
            assert_eq!(queued.owner.owner, captured);
            assert_eq!(queued.owner.owner.basis, basis);
        }
        assert!(queue.next().is_none());
    }

    #[test]
    fn mutation_admission_checks_the_captured_owner_before_any_socket_command() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        for mode in [crate::model::ServiceMode::Embedded, crate::model::ServiceMode::Attached] {
            let basis = crate::core::VersionedRoot::synthetic(backend_library::view_state_root(&[]), 4);
            let gate = super::super::super::owner::OwnerGate::ready(basis, mode);
            let producer = Arc::new(std::sync::RwLock::new(None));
            let composition = crate::host::registry::test_install_registry(&producer, composition(0, "same-source"));
            let owner = WorkOwner::new(owned(&gate, basis), composition.generation, Some(producer));
            let calls = Arc::new(AtomicUsize::new(0));
            let mut session = Session::from_transport(&composition.endpoint, CountedTransport(Arc::clone(&calls)));
            assert!(owner.before_send(&mut session).is_err());
            assert_eq!(calls.load(Ordering::SeqCst), 1, "a current lease reaches the actual revision confirmation");
            gate.publish(super::super::super::owner::OwnerState::Starting);
            gate.publish(super::super::super::owner::OwnerState::Ready { key: basis, mode });
            assert!(owner.before_send(&mut session).is_err());
            assert_eq!(calls.load(Ordering::SeqCst), 1, "a replacement cannot borrow the old request's socket or send");
        }
    }

    #[test]
    fn the_real_drain_skips_replaced_and_withdrawn_jobs_without_source_io() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let queue = held_queue();
        let producer = Arc::new(std::sync::RwLock::new(None));
        let old_calls = Arc::new(AtomicUsize::new(0));
        let current_calls = Arc::new(AtomicUsize::new(0));
        let old_posts = Arc::new(Mutex::new(Vec::new()));
        let current_posts = Arc::new(Mutex::new(Vec::new()));
        let make = |calls| Composition { source: Arc::new(CountedRefusal { calls }), ..composition(0, "same-source") };
        let old = crate::host::registry::test_install_registry(&producer, make(Arc::clone(&old_calls)));
        let old_sink = Arc::clone(&old_posts);
        assert!(queue.push_fixture(Job::Release(Release::new("old", "0.1.0").expect("release")), old,
            Arc::new(move |stage| old_sink.lock().expect("old posts").push(stage)), Arc::clone(&producer)));
        let current = crate::host::registry::test_install_registry(&producer, make(Arc::clone(&current_calls)));
        let current_sink = Arc::clone(&current_posts);
        assert!(queue.push_fixture(Job::Release(Release::new("current", "0.1.0").expect("release")), current,
            Arc::new(move |stage| current_sink.lock().expect("current posts").push(stage)), Arc::clone(&producer)));
        queue.drain();
        assert_eq!(old_calls.load(Ordering::SeqCst), 0, "retired jobs must not resolve their source");
        assert!(old_posts.lock().expect("old posts").is_empty(), "retired jobs must not publish");
        assert_eq!(current_calls.load(Ordering::SeqCst), 1);
        assert!(matches!(current_posts.lock().expect("current posts").last(), Some(Landed::Stage(_, Stage::Failed(_)))));
        assert!(!queue.pending.lock().expect("queue").running);

        let withdrawn_calls = Arc::new(AtomicUsize::new(0));
        let withdrawn = crate::host::registry::test_install_registry(&producer, make(Arc::clone(&withdrawn_calls)));
        queue.pending.lock().expect("queue").running = true;
        let withdrawn_posts = Arc::new(Mutex::new(Vec::new()));
        let withdrawn_sink = Arc::clone(&withdrawn_posts);
        assert!(queue.push_fixture(Job::Release(Release::new("withdrawn", "0.1.0").expect("release")), withdrawn,
            Arc::new(move |stage| withdrawn_sink.lock().expect("withdrawn posts").push(stage)), Arc::clone(&producer)));
        *producer.write().expect("producer") = None;
        queue.drain();
        assert_eq!(withdrawn_calls.load(Ordering::SeqCst), 0);
        assert!(withdrawn_posts.lock().expect("withdrawn posts").is_empty());
        assert!(!queue.pending.lock().expect("queue").running);
    }

    #[test]
    fn queued_jobs_keep_their_own_composition_and_progress_destination() {
        let queue = held_queue();
        let producer = Arc::new(std::sync::RwLock::new(None));
        let release = Release::new("present", "0.4.2").expect("release");
        let first = Arc::new(Mutex::new(Vec::new()));
        let second = Arc::new(Mutex::new(Vec::new()));
        let first_sink = Arc::clone(&first);
        let first_composition = crate::host::registry::test_install_registry(&producer, composition(31, "first-source"));
        assert!(queue.push_fixture(Job::Release(release.clone()), first_composition, Arc::new(move |stage| first_sink.lock().expect("first").push(stage)), Arc::clone(&producer)));
        let first_job = queue.next().expect("first job");
        assert!(first_job.current(first_job.serving_generation()));
        run(&first_job.job, &first_job.composition, first_job.post.as_ref());
        let second_sink = Arc::clone(&second);
        let second_composition = crate::host::registry::test_install_registry(&producer, composition(32, "second-source"));
        assert!(queue.push_fixture(Job::Release(release), second_composition, Arc::new(move |stage| second_sink.lock().expect("second").push(stage)), Arc::clone(&producer)));
        let second_job = queue.next().expect("second job");
        assert!(second_job.current(second_job.serving_generation()));
        assert!(!first_job.current(first_job.serving_generation()));
        assert_eq!(first_job.composition.authority.as_ref(), "first-source");
        assert_eq!(second_job.composition.authority.as_ref(), "second-source");
        run(&second_job.job, &second_job.composition, second_job.post.as_ref());
        for (posted, expected, absent) in [(&first, "first-source", "second-source"), (&second, "second-source", "first-source")] {
            let posted = posted.lock().expect("own progress");
            assert!(matches!(posted.first(), Some(Landed::Stage(_, Stage::Resolving))));
            let Some(Landed::Stage(_, Stage::Failed(words))) = posted.last() else { panic!("own terminal stage missing") };
            assert!(words.contains(expected));
            assert!(!words.contains(absent), "a queued job used the first drainer's source or destination");
        }
    }

    #[test]
    fn a_replaced_queued_job_is_not_admitted_or_deduplicated_against_its_successor() {
        let queue = held_queue();
        let producer = Arc::new(std::sync::RwLock::new(None));
        let job = Job::Release(Release::new("present", "0.4.2").expect("release"));
        let original = composition(41, "same-source");
        let old = crate::host::registry::test_install_registry(&producer, original.clone());
        assert!(queue.push_fixture(job.clone(), old.clone(), Arc::new(|_| {}), Arc::clone(&producer)));
        let next = crate::host::registry::test_install_registry(&producer, original);
        assert_ne!(old.generation, next.generation);
        assert_eq!(old.endpoint, next.endpoint);
        assert_eq!(old.authority, next.authority);
        assert!(queue.push_fixture(job.clone(), next.clone(), Arc::new(|_| {}), Arc::clone(&producer)), "a new owner can queue the same release");
        assert!(!queue.push_fixture(job, next, Arc::new(|_| {}), Arc::clone(&producer)), "the current identical request is still deduplicated");
        let current = queue.next().expect("current priority request");
        assert!(current.current(current.serving_generation()));
        let retired = queue.next().expect("retired request");
        assert!(!retired.current(retired.serving_generation()), "queued work cannot run under the replacement owner");
        *producer.write().expect("producer") = None;
        assert!(!retired.current(retired.serving_generation()), "withdrawal cannot admit queued work");
    }
}

#[cfg(test)]
pub(crate) fn run(job: &Job, composition: &Composition, post: &(dyn Fn(Landed) + Send + Sync)) {
    run_in(job, composition, None, post);
}

fn run_in(job: &Job, composition: &Composition, owner: Option<&WorkOwner>, post: &(dyn Fn(Landed) + Send + Sync)) {
    if check(owner).is_err() { return; }
    match job {
        Job::Release(release) => {
            index_release_in(composition, release, owner, Listed::Ready, &|stage| post(Landed::Stage(release.clone(), stage)));
        }
        Job::Dependencies(project) => {
            let root = project.path();
            let read = if root.join("Cargo.toml").is_file() {
                connect(composition, owner)
                    .map_err(|error| format!("the index could not be reached: {error}"))
                    .and_then(|mut session| dependencies(&mut session, composition.source.as_ref(), &root))
            } else {
                Err(NOT_CARGO.to_owned())
            };
            match read {
                Ok(found) => {
                    let found: Arc<[Dependency]> = found.into();
                    // What the owner already lists lands before the list is
                    // drawn: a relaunch shows its packages in the library from
                    // its first frame, never "adding" them again.
                    if check(owner).is_err() { return; }
                    let settled = listed_already_in(composition, &found, owner);
                    for (release, stage) in &settled {
                        post(Landed::Stage(release.clone(), stage.clone()));
                    }
                    post(Landed::Dependencies(project.clone(), Ok(Arc::clone(&found))));
                    for dependency in found.iter().filter(|dependency| matches!(dependency.origin, Origin::Registry(_))) {
                        let release = &dependency.release;
                        if settled.iter().any(|(settled, _)| settled == release) {
                            continue;
                        }
                        reads_first();
                        // A package the owner already lists, in any state, was
                        // read once: a project indexed again does not compile
                        // it again (a refusal stays the owner's).
                        index_release_in(composition, release, owner, Listed::Any, &|stage| post(Landed::Stage(release.clone(), stage)));
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
/// `Cargo.lock`). This target/features-filtered graph does not identify why
/// other locked package rows are inactive.
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
            let origin = dependency_origin(&package.origin, &release, source);
            Some(Dependency { release, direct: package.role == PackageRole::Direct, origin })
        })
        .collect()
}

pub(crate) fn dependency_origin(origin: &PackageOrigin, release: &Release, source: &dyn RegistrySource) -> Origin {
    match origin {
        PackageOrigin::Registry { .. } if origin.is_crates_io_registry() => Origin::Registry(source.availability(release)),
        PackageOrigin::Registry { source } => Origin::Elsewhere(Arc::from(format!(
            "from registry {source}: the configured source only offers crates.io releases"
        ))),
        PackageOrigin::Git { .. } => Origin::Elsewhere(Arc::from("from a Git source: only registry releases are added")),
        PackageOrigin::Vendored { path } => Origin::Elsewhere(Arc::from(format!("vendored at {path}: only registry releases are added"))),
        PackageOrigin::Unresolved { source } => Origin::Elsewhere(Arc::from(format!(
            "source {} is unresolved: no release is offered",
            source.as_deref().unwrap_or("not recorded")
        ))),
    }
}

/// Each of `found`'s registry packages the owner already lists (in any state,
/// from one read of its list), with the stage it stands at: added, or thin
/// for the reason the owner gave when it could not finish the compile. Only
/// trees already on disk are looked at; nothing is unpacked or indexed.
#[cfg(test)]
pub(crate) fn listed_already(composition: &Composition, found: &[Dependency]) -> Vec<(Release, Stage)> {
    listed_already_in(composition, found, None)
}

fn listed_already_in(composition: &Composition, found: &[Dependency], owner: Option<&WorkOwner>) -> Vec<(Release, Stage)> {
    let Ok(mut session) = connect(composition, owner) else { return Vec::new() };
    let Ok(reply) = session.packages() else { return Vec::new() };
    let backend_library::CommandReply::Packages(snapshot) = reply.reply else { return Vec::new() };
    let listed = snapshot.root.rows().iter().map(|row| row.label.clone()).collect::<std::collections::HashSet<_>>();
    let refusals = composition.refusals.as_deref().map(Refusals::at);
    found
        .iter()
        .filter_map(|dependency| {
            let Origin::Registry(Availability::Unpacked(tree)) = &dependency.origin else { return None };
            let tree = tree.canonicalize().ok()?;
            let coordinate = tree.to_str()?;
            if !listed.contains(coordinate) {
                return None;
            }
            let package = PackageRef::parse(coordinate).ok()?;
            let stage = match refusals.as_ref().and_then(|refusals| refusals.words(coordinate)) {
                Some(words) => Stage::Partial { page: package, words },
                None => Stage::Added(package),
            };
            Some((dependency.release.clone(), stage))
        })
        .collect()
}

/// Resolves `release` through the source and has the owner index its tree,
/// posting each stage it reaches; returns the last one. A release the owner
/// already lists as `skip` says is not indexed again.
#[cfg(test)]
pub(crate) fn index_release(composition: &Composition, release: &Release, skip: Listed, post: &dyn Fn(Stage)) -> Stage {
    index_release_in(composition, release, None, skip, post)
}

fn index_release_in(composition: &Composition, release: &Release, owner: Option<&WorkOwner>, skip: Listed, post: &dyn Fn(Stage)) -> Stage {
    let finish = |stage: Stage| {
        post(stage.clone());
        stage
    };
    let failed = |words: String| finish(Stage::Failed(Arc::from(words)));
    if let Err(error) = check(owner) { return failed(error.to_string()); }
    post(Stage::Resolving);
    let availability = composition.source.availability(release);
    if let Err(error) = check(owner) { return failed(error.to_string()); }
    match availability {
        Availability::Unpacked(_) => {}
        Availability::Archive(_) => post(Stage::Unpacking),
        Availability::Download => {
            return add_published_release(composition, release, owner, skip, post);
        }
        Availability::Ambiguous { indexes } => {
            let indexes = indexes
                .iter()
                .map(|index| index.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            return failed(format!(
                "{release} appears under conflicting Cargo registry sources ({indexes}). Set NUDOX_CARGO_ROOT to the intended source root, or NUDOX_CARGO_HOME to the intended Cargo home, then restart."
            ));
        }
        Availability::UnverifiedArchive(path) => {
            return failed(format!(
                "the local archive {} has no trusted registry checksum, so it cannot be unpacked; use a verified Cargo archive or resolve the registry authority first",
                path.display()
            ));
        }
    }
    let tree = match composition.source.resolve(release) {
        Ok(tree) => tree,
        Err(error) => return failed(error.to_string()),
    };
    if let Err(error) = check(owner) { return failed(error.to_string()); }
    let Some(coordinate) = tree.root.to_str() else { return failed(format!("{} is not UTF-8", tree.root.display())) };
    let started = std::time::Instant::now();
    let refusals = composition.refusals.as_deref().map(Refusals::at);
    let mut listed_already = false;
    let indexed = connect(composition, owner).and_then(|mut session| {
        if is_listed(&mut session, coordinate, skip) {
            listed_already = true;
            return Ok(());
        }
        before_send(owner, &mut session)?;
        post(Stage::Indexing);
        session.index(coordinate).map(|_| ())
    });
    crate::runtime::trace::span("acquire.index", started, format_args!("{release}"));
    if let Err(error) = check(owner) { return failed(error.to_string()); }
    match (indexed, PackageRef::parse(coordinate)) {
        // Listed by an earlier launch: what the owner said then, if it could
        // not finish the compile, still stands.
        (Ok(()), Ok(package)) if listed_already => match refusals.as_ref().and_then(|refusals| refusals.words(coordinate)) {
            Some(words) => finish(Stage::Partial { page: package, words }),
            None => finish(Stage::Added(package)),
        },
        (Ok(()), Ok(package)) => {
            if let Err(error) = check(owner) { return failed(error.to_string()); }
            if let Some(refusals) = &refusals {
                refusals.forget(coordinate);
            }
            finish(Stage::Added(package))
        }
        // A compile the owner refused still lists the release, on the names
        // its source declares: it is in the library, and says why it is thin.
        (Err(error), Ok(package))
            if connect(composition, owner).is_ok_and(|mut session| is_listed(&mut session, coordinate, Listed::Any)) =>
        {
            if let Err(error) = check(owner) { return failed(error.to_string()); }
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

/// Sends an explicitly requested published release through the owner's
/// configured RegistryGateway. The owner enforces its online/offline policy,
/// archive budget, verification, and compiler path; the desktop has no
/// second network fetcher.
fn add_published_release(
    composition: &Composition,
    release: &Release,
    owner: Option<&WorkOwner>,
    skip: Listed,
    post: &dyn Fn(Stage),
) -> Stage {
    let purl = release.purl();
    let mut session = match connect(composition, owner) {
        Ok(session) => session,
        Err(error) => {
            post(Stage::Failed(Arc::from(format!(
                "the local index could not be reached: {error}"
            ))));
            return Stage::Failed(Arc::from(format!(
                "the local index could not be reached: {error}"
            )));
        }
    };
    match exact_listed_release(&mut session, release, composition) {
        Ok(Some(package)) if is_listed(&mut session, package.as_str(), skip) => {
            if let Err(error) = check(owner) {
                let stage = Stage::Failed(Arc::from(error.to_string()));
                post(stage.clone());
                return stage;
            }
            let coordinate = package.as_str().to_owned();
            return finish_already_listed(composition, package, &coordinate, post);
        }
        Ok(_) => {}
        Err(reason) => {
            let failed: Arc<str> = Arc::from(reason);
            post(Stage::Failed(Arc::clone(&failed)));
            return Stage::Failed(failed);
        }
    }

    if let Err(error) = before_send(owner, &mut session) {
        let stage = Stage::Failed(Arc::from(error.to_string()));
        post(stage.clone());
        return stage;
    }
    post(Stage::Indexing);
    let indexed = session.index_with_execution_intent(
        &purl,
        backend_library::CompileExecutionIntent::Interactive,
    );
    if let Err(error) = check(owner) {
        let stage = Stage::Failed(Arc::from(error.to_string()));
        post(stage.clone());
        return stage;
    }
    let listed = exact_listed_release(&mut session, release, composition);
    if let Err(error) = check(owner) {
        let stage = Stage::Failed(Arc::from(error.to_string()));
        post(stage.clone());
        return stage;
    }
    match (indexed, listed) {
        (Ok(_), Ok(Some(package))) => {
            if let Some(refusals) = composition.refusals.as_deref() {
                Refusals::at(refusals).forget(package.as_str());
            }
            let stage = Stage::Added(package);
            post(stage.clone());
            stage
        }
        (Err(error), Ok(Some(package))) => {
            let words: Arc<str> = Arc::from(error.to_string());
            if let Some(refusals) = composition.refusals.as_deref() {
                Refusals::at(refusals).keep(package.as_str(), &words);
            }
            let stage = Stage::Partial {
                page: package,
                words,
            };
            post(stage.clone());
            stage
        }
        (Err(error), Ok(None)) => {
            let stage = Stage::Failed(Arc::from(format!(
                "the local service could not add {release}: {error}"
            )));
            post(stage.clone());
            stage
        }
        (Ok(_), Ok(None)) => {
            let stage = Stage::Failed(Arc::from(format!(
                "the owner accepted {release}, but the exact package is not in its current index"
            )));
            post(stage.clone());
            stage
        }
        (_, Err(reason)) => {
            let stage = Stage::Failed(Arc::from(reason));
            post(stage.clone());
            stage
        }
    }
}

fn finish_already_listed(
    composition: &Composition,
    package: PackageRef,
    coordinate: &str,
    post: &dyn Fn(Stage),
) -> Stage {
    let refusals = composition.refusals.as_deref().map(Refusals::at);
    let stage = match refusals.as_ref().and_then(|refusals| refusals.words(coordinate)) {
        Some(words) => Stage::Partial {
            page: package,
            words,
        },
        None => Stage::Added(package),
    };
    post(stage.clone());
    stage
}

/// Reads the owner's exact package rows and resolves a release only when a
/// single package identity names that exact registry release.
fn exact_listed_release(
    session: &mut Session,
    release: &Release,
    composition: &Composition,
) -> Result<Option<PackageRef>, String> {
    let reply = session
        .packages()
        .map_err(|error| format!("could not verify the owner's package listing: {error}"))?;
    let backend_library::CommandReply::Packages(snapshot) = reply.reply else {
        return Err("the owner returned an unexpected package listing".to_owned());
    };
    let mut exact = snapshot
        .root
        .rows()
        .iter()
        .filter_map(|row| {
            let package = PackageRef::parse(&row.label).ok()?;
            let identity = package.release().or_else(|| {
                composition
                    .source
                    .release_of(Path::new(package.as_str()))
            });
            (identity.as_ref() == Some(release)).then_some(package)
        })
        .collect::<Vec<_>>();
    exact.sort();
    exact.dedup();
    match exact.as_slice() {
        [] => Ok(None),
        [only] => Ok(Some(only.clone())),
        _ => Err(format!(
            "the owner lists {} distinct source roots for exact release {release}; resolve the Cargo registry authority before adding it",
            exact.len()
        )),
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
