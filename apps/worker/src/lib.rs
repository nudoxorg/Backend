//! Pure worker process adapter.
//!
//! The process owns a capability registry and a pure recipe executor only.
//! Workspace heads, durable journals, external effects, and view publication
//! remain in `locald`/`backend-engine`.
#![deny(unsafe_code)]

pub mod builtin;
mod closure_index;
pub mod input_cas;
pub mod listener;
pub mod process;
pub mod protocol;
pub mod service;

use backend_engine::{
    NoAttestationSigner, PureRecipeExecutor, TransportLimits, WorkerAttestationSigner,
    WorkerCapabilities, WorkerEndpoint, WorkerError,
};
use std::fmt;

pub use input_cas::{CasGcLimits, CasGcReport, CasRootLease, InputCas};
pub use listener::{
    FilesystemPeerPolicy, PeerPolicy, PeerPolicyError, TcpExposure, TcpWorkerListener,
    TcpWorkerListenerConfig, UnixWorkerListener, WorkerListenerConfig, WorkerListenerError,
    WorkerRunReport,
};
pub use process::{
    AUTHORITY_SECRET_ENV, PROFILE_ENV, WorkerProcessConfig, WorkerProcessError, main_entry,
    run_process, run_with_service,
};
pub use protocol::{
    WorkerFramed, WorkerLimits, WorkerProtocolError, frame, read_message, write_message,
};
pub use service::{
    ActiveJobKey, CancellationHandle, JobAdmission, JobCancellation, NoJobAdmission, WorkerJob,
    WorkerJobBindings, WorkerService,
};

/// Capability configuration loaded by the worker process.
pub type Capabilities = WorkerCapabilities;

/// Worker process endpoint. It contains no durable owner or effect sink.
pub struct Worker<E: PureRecipeExecutor, S: WorkerAttestationSigner = NoAttestationSigner> {
    endpoint: WorkerEndpoint<E, S>,
}

impl<E: PureRecipeExecutor, S: WorkerAttestationSigner> fmt::Debug for Worker<E, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Worker")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl<E: PureRecipeExecutor> Worker<E, NoAttestationSigner> {
    /// Creates a worker around a pure executor and negotiated limits.
    #[must_use]
    pub fn new(capabilities: Capabilities, executor: E, limits: TransportLimits) -> Self {
        Self {
            endpoint: WorkerEndpoint::new(capabilities, executor, limits),
        }
    }
}

impl<E: PureRecipeExecutor, S: WorkerAttestationSigner> Worker<E, S> {
    /// Creates a worker with an owner-supplied attestation signer.
    #[must_use]
    pub fn with_signer(
        capabilities: Capabilities,
        executor: E,
        signer: S,
        limits: TransportLimits,
    ) -> Self {
        Self {
            endpoint: WorkerEndpoint::with_signer(capabilities, executor, signer, limits),
        }
    }

    /// Returns the pure engine endpoint.
    #[must_use]
    pub const fn endpoint(&self) -> &WorkerEndpoint<E, S> {
        &self.endpoint
    }

    /// Returns immutable advertised capabilities.
    #[must_use]
    pub fn capabilities(&self) -> &Capabilities {
        self.endpoint.capabilities()
    }

    /// Consumes the wrapper and returns the configured pure endpoint.
    #[must_use]
    pub fn into_endpoint(self) -> WorkerEndpoint<E, S> {
        self.endpoint
    }
}

/// Worker process error alias.
pub type Error = WorkerError;
