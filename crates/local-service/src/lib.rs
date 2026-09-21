//! Local daemon adapter.
//!
//! `locald` owns one [`backend_engine::Engine`] instance and exposes only a
//! bounded client transport. Clients cannot read or mutate heads directly;
//! all state transitions run through the engine owner loop.
#![deny(unsafe_code)]

pub mod builtin;
mod embedded;
pub mod listener;
pub mod process;
pub mod protocol;
/// Durable registry acquisition is an engine effect composed by local-service.
pub use backend_engine::registry;
pub(crate) mod reconcile;
pub mod service;
pub(crate) mod worker_transport;

use backend_engine::{
    DaemonConfig, DaemonError, DaemonHandle, DaemonReply, DaemonRequest, Dispatcher, Engine,
    QueueError, RemoteAuthorityPolicy, TransportMessage, WorkspaceError, WorkspaceHead,
    WorkspaceModel,
};
use std::fmt;
use std::path::Path;

pub use embedded::{EmbeddedLocalService, ServiceStart, start_or_attach};
pub use listener::{
    DEFAULT_IDLE_TIMEOUT, FilesystemPeerPolicy, ListenerConfig, ListenerError, ListenerShutdown,
    PeerPolicy, PeerPolicyError, RunReport, UnixListenerService,
};
pub use process::{
    ADVISORY_GHSA_ENV, ADVISORY_MAX_AGE_ENV, ADVISORY_OFFLINE_ENV, ADVISORY_OSV_ENV,
    ADVISORY_POLICY_ENV, ADVISORY_RUSTSEC_ENV, AUTHORITY_SECRET_ENV, AdvisoryConfig,
    AdvisorySourceConfig, ENDPOINT_ENV, PROFILE_ENV, ProcessConfig, ProcessError,
    REGISTRY_AUTH_ENV, REGISTRY_AUTH_FILE_ENV, REGISTRY_AUTH_SCOPES_ENV, REGISTRY_ECOSYSTEM_ENV,
    REGISTRY_ENDPOINT_ENV, REGISTRY_NATIVE_ENV, REGISTRY_OFFLINE_ENV, REGISTRY_SOURCES_ENV,
    RegistryConfig, WORKER_ENDPOINT_ENV, WORKSPACE_ENV,
    main_entry, run_process, run_with_owner,
};
pub use protocol::{
    CompletionClaim, EngineRequest, EngineStatus, FrameLimits, LIFECYCLE_BYTES, LIFECYCLE_MAGIC,
    LIFECYCLE_VERSION, Operation, ProtocolError, RequestFrame, ResponseFrame,
    decode_engine_request, decode_request, decode_response, encode_engine_request, encode_response,
    frame, is_lifecycle, read_frame, unframe, write_frame,
};
pub use service::{
    CompletionAdmission, LocaldOwner, LocaldService, NoCompletionAdmission, NoReplicationAdmission,
    OwnerService, ReplicationAdmission, ServiceError,
};

/// Versioned request body sent through the local transport boundary.
#[derive(Clone, Debug)]
pub enum Request<I> {
    /// Explicit durable workspace intent.
    Commit {
        /// Client idempotency identity.
        request: [u8; 32],
        /// Expected current root and sequence.
        expected: backend_engine::HeadExpectation,
        /// Typed intent.
        intent: I,
    },
    /// Immutable snapshot query.
    Query,
    /// Replication/control transport message.
    Replicate(Box<TransportMessage>),
    /// Already-admitted worker completion.
    Complete(backend_engine::daemon::CompletionNotice),
    /// Cursor subscription request.
    Subscribe {
        /// Versioned cursor bytes.
        cursor: Box<[u8]>,
        /// Bounded event credit.
        credit: usize,
    },
}

impl<I> From<Request<I>> for DaemonRequest<I> {
    fn from(request: Request<I>) -> Self {
        match request {
            Request::Commit {
                request,
                expected,
                intent,
            } => Self::Commit {
                request,
                expected,
                intent,
            },
            Request::Query => Self::Query,
            Request::Replicate(message) => Self::Replicate(message),
            Request::Complete(notice) => Self::Complete(notice),
            Request::Subscribe { cursor, credit } => Self::Subscribe { cursor, credit },
        }
    }
}

/// Response returned by the local transport.
pub type Response = DaemonReply;

/// Local daemon process wrapper. It is intentionally non-`Clone`; only its
/// client handles can be cloned.
pub struct Locald<
    M: WorkspaceModel,
    V = backend_engine::UnconfiguredOutputValidator,
    A = backend_engine::UnconfiguredAuthorityVerifier,
> where
    V: backend_engine::OutputAdmissionValidator + backend_engine::SemanticCoverageValidator,
    A: backend_engine::AttestationVerifier + Send + Sync + 'static,
{
    engine: Engine<M, V, A>,
}

impl<M: WorkspaceModel, V, A> fmt::Debug for Locald<M, V, A>
where
    V: backend_engine::OutputAdmissionValidator + backend_engine::SemanticCoverageValidator,
    A: backend_engine::AttestationVerifier + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Locald")
            .field("engine", &self.engine)
            .finish()
    }
}

impl<M: WorkspaceModel> Locald<M> {
    /// Opens the workspace and acquires its durable owner lock.
    ///
    /// # Errors
    ///
    /// Returns an error when the workspace cannot be opened or locked.
    pub fn open(
        path: impl AsRef<Path>,
        model: M,
        genesis: WorkspaceHead,
        config: DaemonConfig,
    ) -> Result<Self, LocaldError> {
        let dispatcher = Dispatcher::new(
            backend_engine::Budget::default(),
            backend_engine::UnconfiguredOutputValidator,
            backend_engine::UnconfiguredAuthorityVerifier,
            RemoteAuthorityPolicy::LocalOnly,
        );
        Engine::open(path, model, genesis, dispatcher, config)
            .map(|engine| Self { engine })
            .map_err(LocaldError::Workspace)
    }
}

impl<M, V, A> Locald<M, V, A>
where
    M: WorkspaceModel,
    V: backend_engine::OutputAdmissionValidator + backend_engine::SemanticCoverageValidator,
    A: backend_engine::AttestationVerifier + Send + Sync + 'static,
{
    /// Opens locald around an explicitly configured dispatcher. This is the
    /// production constructor; validators, authority policy, and scheduler
    /// budgets are supplied by the engine composition root.
    ///
    /// # Errors
    ///
    /// Returns an error when the workspace cannot be opened or locked.
    pub fn open_with_dispatcher(
        path: impl AsRef<Path>,
        model: M,
        genesis: WorkspaceHead,
        dispatcher: Dispatcher<V, A>,
        config: DaemonConfig,
    ) -> Result<Self, LocaldError> {
        Engine::open(path, model, genesis, dispatcher, config)
            .map(|engine| Self { engine })
            .map_err(LocaldError::Workspace)
    }

    /// Opens locald with a dispatcher and the relation registry required by
    /// its durable product model.
    ///
    /// # Errors
    ///
    /// Returns an error when the workspace or relation registry is invalid.
    pub fn open_with_dispatcher_and_registry(
        path: impl AsRef<Path>,
        model: M,
        genesis: WorkspaceHead,
        dispatcher: Dispatcher<V, A>,
        config: DaemonConfig,
        relation_registry: backend_engine::RelationAdmissionRegistry,
    ) -> Result<Self, LocaldError> {
        Engine::open_with_relation_registry(
            path,
            model,
            genesis,
            dispatcher,
            config,
            relation_registry,
        )
        .map(|engine| Self { engine })
        .map_err(LocaldError::Workspace)
    }

    /// Returns a bounded client transport handle.
    #[must_use]
    pub fn client(&self) -> Client<M::Intent> {
        Client {
            handle: self.engine.handle(),
        }
    }

    /// Runs one fair owner-loop operation.
    pub fn serve_one(&mut self) -> bool {
        self.engine.serve_one()
    }

    /// Returns the sole owner for controlled shutdown.
    #[must_use]
    pub fn engine(&self) -> &Engine<M, V, A> {
        &self.engine
    }

    /// Returns a mutable reference to the engine for owner-loop composition.
    /// Transport setup and retained dispatch admission must happen before the
    /// daemon is handed to the listener service, preserving one owner.
    #[must_use]
    pub fn engine_mut(&mut self) -> &mut Engine<M, V, A> {
        &mut self.engine
    }

    /// Installs the daemon-owned remote transport path before serving clients.
    pub fn set_remote_transport(&mut self, transport: Box<dyn backend_engine::RemoteTransport>) {
        self.engine.set_remote_transport(transport);
    }

    /// Closes all request lanes.
    pub fn close(&mut self) {
        self.engine.daemon_mut().close();
    }

    /// Converts this already-open daemon into an owner-loop service adapter.
    /// The command callback is the engine composition seam for strict
    /// library DTO admission and execution; it receives the raw command body
    /// only after outer framing checks.
    pub fn into_owner<F>(self, command: F) -> LocaldOwner<M, V, A, F> {
        LocaldOwner::new(self, command)
    }

    /// Converts this daemon into a service adapter with an explicit completion
    /// admission seam. Raw completion claims never become engine notices
    /// without this owner-supplied callback.
    pub fn into_owner_with_completion<F, C>(
        self,
        command: F,
        completion: C,
    ) -> LocaldOwner<M, V, A, F, C> {
        LocaldOwner::with_completion(self, command, completion)
    }

    /// Converts this daemon into a service adapter with retained dispatch
    /// admission for worker results as well as completion claims.
    pub fn into_owner_with_admission<F, C, R>(
        self,
        command: F,
        completion: C,
        replication: R,
    ) -> LocaldOwner<M, V, A, F, C, R> {
        LocaldOwner::with_admission(self, command, completion, replication)
    }
}

/// Cloneable client transport handle. A client owns demand/waiter references,
/// never a workspace head.
pub struct Client<I: backend_engine::QueueSized> {
    handle: DaemonHandle<I>,
}

impl<I: backend_engine::QueueSized> Clone for Client<I> {
    fn clone(&self) -> Self {
        Self {
            handle: self.handle.clone(),
        }
    }
}

impl<I: Send + backend_engine::QueueSized + 'static> Client<I> {
    /// Sends one request to the bounded local queue.
    ///
    /// # Errors
    ///
    /// Returns an error when the queue is closed or over capacity.
    pub fn request(
        &self,
        request_id: u64,
        request: Request<I>,
    ) -> Result<std::sync::mpsc::Receiver<Response>, LocaldError> {
        self.handle
            .submit(request_id, request.into())
            .map_err(LocaldError::Queue)
    }
}

/// Local daemon startup and queue error.
#[derive(Debug)]
pub enum LocaldError {
    /// Workspace opening/recovery failed.
    Workspace(WorkspaceError),
    /// Queue admission failed.
    Queue(QueueError),
    /// Owner loop returned a daemon error.
    Daemon(DaemonError),
}

impl fmt::Display for LocaldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "locald error: {self:?}")
    }
}
impl std::error::Error for LocaldError {}
