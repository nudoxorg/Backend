//! Admission-bound worker job state and cancellation lifecycle.
//!
//! This module contains the typed bindings retained by the worker owner and
//! the single execution adapter used by both synchronous and streaming
//! service entry points.

use backend_engine::worker::{
    AdmittedInput, PureRecipeExecutor, WorkerAttestationSigner, WorkerEndpoint,
};
use backend_engine::{
    AttemptId, AuthorityVersion, CancelAttemptExpectation, CancelHandle, Cancellation,
    CancellationId, CompleteSemanticCoverage, ExecutionRequestExpectation, Fence,
    OutputEquivalence, ReadManifestId, RecipeId, Relation, ResourceEnvelope, RevocationVersion,
    WireIdentity, WireRecipeRequest, WorkKey, WorkerError, WorkspaceRoot,
};
use std::fmt;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

/// Typed execution material retained by an owner before one worker attempt.
#[derive(Clone, Debug)]
pub struct WorkerJobBindings<R: Relation> {
    /// Exact request expectation retained by the scheduler.
    pub expected: ExecutionRequestExpectation,
    /// Typed workspace input basis.
    pub input_basis: WorkspaceRoot,
    /// Typed registered recipe identity.
    pub recipe: RecipeId,
    /// Typed read manifest identity.
    pub read_manifest: ReadManifestId,
    /// Typed worker authority identity.
    pub authority: AuthorityVersion,
    /// Declared output equivalence contract.
    pub output_equivalence: OutputEquivalence,
    /// Exact semantic work identity.
    pub work_key: WorkKey,
    /// Input objects admitted from the local transfer/store adapter.
    pub inputs: Box<[AdmittedInput]>,
    /// Owner-admitted complete semantic coverage.
    pub semantic: CompleteSemanticCoverage,
    /// Current revocation observation.
    pub observed_revocation: RevocationVersion,
    /// Hard resource envelope for this attempt.
    pub resources: ResourceEnvelope,
    /// Relation marker tying the work identity to the selected input root.
    pub relation: core::marker::PhantomData<R>,
}

/// An affine cancellation observation and its matching owner handle.
///
/// The pair is created together and is consumed by [`WorkerJob::with_cancellation`],
/// so an admission implementation cannot accidentally pass a handle from one
/// attempt with an observation from another.
#[derive(Debug)]
pub struct JobCancellation {
    observation: Cancellation,
    handle: CancellationHandle,
}

impl JobCancellation {
    /// Creates a matching cancellation observation and owner handle.
    #[must_use]
    pub fn new() -> Self {
        let (observation, handle) = Cancellation::new();
        Self {
            observation,
            handle: CancellationHandle::from_handle(handle),
        }
    }

    /// Returns the observation supplied to the pure endpoint.
    #[must_use]
    pub const fn observation(&self) -> &Cancellation {
        &self.observation
    }

    /// Returns the owner handle for an authenticated cancellation control.
    #[must_use]
    pub const fn handle(&self) -> &CancellationHandle {
        &self.handle
    }
}

impl Default for JobCancellation {
    fn default() -> Self {
        Self::new()
    }
}

/// All typed material an execution owner must supply before a worker can run
/// one wire request.
#[derive(Debug)]
pub struct WorkerJob<R: Relation> {
    /// Exact request bindings retained by the scheduler.
    pub bindings: WorkerJobBindings<R>,
    /// Matching scheduler cancellation observation and owner handle.
    pub cancellation: JobCancellation,
}

impl<R: Relation> WorkerJob<R> {
    /// Binds typed execution material to its matching cancellation pair.
    #[must_use]
    pub fn with_cancellation(
        bindings: WorkerJobBindings<R>,
        cancellation: JobCancellation,
    ) -> Self {
        Self {
            bindings,
            cancellation,
        }
    }
}

/// Cloneable owner handle retained for one active worker attempt.
#[derive(Clone)]
pub struct CancellationHandle(Arc<CancelHandle>);

impl CancellationHandle {
    pub(super) fn from_handle(handle: CancelHandle) -> Self {
        Self(Arc::new(handle))
    }

    /// Requests cooperative cancellation of the associated pure recipe.
    pub fn cancel(&self) {
        self.0.cancel();
    }
}

impl fmt::Debug for CancellationHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CancellationHandle(..)")
    }
}

/// Exact key retained while one remote recipe attempt is executing.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ActiveJobKey {
    /// Scheduler attempt ordinal.
    pub attempt: AttemptId,
    /// Exact semantic work-key claim bytes and context.
    pub work_key: WireIdentity,
    /// Exact scheduler publication fence.
    pub fence: Fence,
    /// Exact scheduler cancellation identity.
    pub cancellation: CancellationId,
}

impl ActiveJobKey {
    /// Derives the key from the complete untrusted request envelope. The
    /// service compares all fields again during typed execution admission.
    #[must_use]
    pub const fn from_request(request: &WireRecipeRequest) -> Self {
        Self {
            attempt: request.attempt,
            work_key: request.work_key,
            fence: request.fence,
            cancellation: request.cancellation,
        }
    }
}

pub(super) struct CompletedJob {
    pub(super) key: ActiveJobKey,
    pub(super) result: Result<backend_engine::WireRecipeResult, WorkerError>,
}

pub(super) struct ActiveJob {
    pub(super) handle: CancellationHandle,
    pub(super) cancellation: CancelAttemptExpectation,
}

pub(super) struct RunningJob {
    pub(super) key: ActiveJobKey,
    pub(super) handle: CancellationHandle,
    pub(super) worker: thread::JoinHandle<()>,
    pub(super) completed: mpsc::Receiver<CompletedJob>,
}

pub(super) fn execute_job<E, S, R>(
    endpoint: &WorkerEndpoint<E, S>,
    request: WireRecipeRequest,
    job: WorkerJob<R>,
) -> Result<backend_engine::WireRecipeResult, WorkerError>
where
    E: PureRecipeExecutor,
    S: WorkerAttestationSigner,
    R: Relation,
{
    let WorkerJob {
        bindings,
        cancellation,
    } = job;
    let WorkerJobBindings {
        expected,
        input_basis,
        recipe,
        read_manifest,
        authority,
        output_equivalence,
        work_key,
        inputs,
        semantic,
        observed_revocation,
        resources,
        relation: _,
    } = bindings;
    endpoint.execute_owned(
        request,
        &expected,
        input_basis,
        recipe,
        read_manifest,
        authority,
        output_equivalence,
        work_key,
        inputs,
        semantic,
        observed_revocation,
        cancellation.observation(),
        resources,
    )
}
