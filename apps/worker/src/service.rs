//! Registered pure-recipe worker service.
//!
//! The service keeps the untrusted wire request separate from the typed job
//! admission supplied by an execution owner. A recipe is runnable only when
//! its exact wire identity appears in the configured capability manifest and
//! the owner admission seam supplies all expected roots, authorities,
//! inputs, coverage, cancellation, and resource limits.

use crate::protocol::{WorkerFramed, WorkerLimits, WorkerProtocolError};
use backend_engine::worker::{PureRecipeExecutor, WorkerAttestationSigner, WorkerEndpoint};
use backend_engine::{
    CancelAttempt, CancelAttemptExpectation, CapabilityManifest, Relation, TransportMessage,
    WireRecipeRequest,
};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

#[path = "service/admission.rs"]
mod admission;
#[path = "service/lifecycle.rs"]
mod lifecycle;
#[path = "service/stream.rs"]
mod stream;

pub use admission::{JobAdmission, NoJobAdmission};
use lifecycle::{ActiveJob, CompletedJob, RunningJob, execute_job};
pub use lifecycle::{
    ActiveJobKey, CancellationHandle, JobCancellation, WorkerJob, WorkerJobBindings,
};

/// A bounded pure worker endpoint.
pub struct WorkerService<
    E: PureRecipeExecutor,
    S: WorkerAttestationSigner = backend_engine::worker::NoAttestationSigner,
> {
    endpoint: Arc<WorkerEndpoint<E, S>>,
    manifest: CapabilityManifest,
    limits: WorkerLimits,
    closed: bool,
    active: BTreeMap<ActiveJobKey, ActiveJob>,
    /// Exact cancellation identity for the most recently published result on
    /// this serial worker. A peer may race its advisory cancellation with the
    /// result frame; retaining one fixed-size expectation lets that late
    /// control be consumed once without weakening active-job admission.
    completed_cancellation: Option<CancelAttemptExpectation>,
}

fn validate_composition(
    capabilities: &backend_engine::WorkerCapabilities,
    manifest: &CapabilityManifest,
    limits: WorkerLimits,
) -> Result<(), WorkerProtocolError> {
    limits.validate()?;
    manifest
        .validate(limits.transport)
        .map_err(WorkerProtocolError::Replication)?;
    if manifest.recipes.is_empty() {
        return Err(WorkerProtocolError::Rejected(
            "worker has no registered pure recipes".to_owned(),
        ));
    }
    if manifest.recipes.iter().any(|advertised| {
        !capabilities
            .recipes
            .iter()
            .any(|recipe| advertised.recipe == backend_engine::WireIdentity::from_typed(recipe))
    }) {
        return Err(WorkerProtocolError::Rejected(
            "worker manifest advertises an unregistered recipe".to_owned(),
        ));
    }
    if !manifest
        .max_resources
        .fits_within(capabilities.max_resources)
    {
        return Err(WorkerProtocolError::Rejected(
            "worker manifest exceeds executable resource limits".to_owned(),
        ));
    }
    Ok(())
}

impl<E, S> fmt::Debug for WorkerService<E, S>
where
    E: PureRecipeExecutor,
    S: WorkerAttestationSigner,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkerService")
            .field("endpoint", &self.endpoint)
            .field("manifest", &self.manifest)
            .field("limits", &self.limits)
            .field("closed", &self.closed)
            .field("active_jobs", &self.active.len())
            .finish_non_exhaustive()
    }
}

impl<E: PureRecipeExecutor> WorkerService<E> {
    /// Creates a nonpublishable worker with an explicit registered capability
    /// manifest. The manifest is mandatory so an empty or implicit recipe
    /// cannot be selected accidentally.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn new(
        capabilities: backend_engine::WorkerCapabilities,
        executor: E,
        manifest: CapabilityManifest,
        limits: WorkerLimits,
    ) -> Result<Self, WorkerProtocolError> {
        validate_composition(&capabilities, &manifest, limits)?;
        Ok(Self {
            endpoint: Arc::new(WorkerEndpoint::new(
                capabilities,
                executor,
                limits.transport,
            )),
            manifest,
            limits,
            closed: false,
            active: BTreeMap::new(),
            completed_cancellation: None,
        })
    }
}

impl<E: PureRecipeExecutor, S: WorkerAttestationSigner> WorkerService<E, S> {
    /// Creates a worker with an owner-supplied attestation signer.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn with_signer(
        capabilities: backend_engine::WorkerCapabilities,
        executor: E,
        signer: S,
        manifest: CapabilityManifest,
        limits: WorkerLimits,
    ) -> Result<Self, WorkerProtocolError> {
        validate_composition(&capabilities, &manifest, limits)?;
        Ok(Self {
            endpoint: Arc::new(WorkerEndpoint::with_signer(
                capabilities,
                executor,
                signer,
                limits.transport,
            )),
            manifest,
            limits,
            closed: false,
            active: BTreeMap::new(),
            completed_cancellation: None,
        })
    }

    /// Returns the advertised capabilities. This is the sole recipe registry
    /// consulted by the wire admission path.
    #[must_use]
    pub const fn manifest(&self) -> &CapabilityManifest {
        &self.manifest
    }

    /// Returns the endpoint's pure executor capabilities.
    #[must_use]
    pub fn endpoint(&self) -> &WorkerEndpoint<E, S> {
        self.endpoint.as_ref()
    }

    /// Returns whether a recipe claim is registered for this process.
    #[must_use]
    pub fn is_registered(&self, request: &WireRecipeRequest) -> bool {
        self.manifest
            .recipes
            .iter()
            .any(|recipe| recipe.recipe == request.recipe)
    }

    /// Returns the number of currently executing stream jobs.
    #[must_use]
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn active_jobs(&self) -> usize {
        self.active.len()
    }

    /// Cancels one exact active attempt after its caller has authenticated the
    /// control frame. A stale or unmatched key is rejected so cancellation
    /// cannot affect a later attempt that reuses a work identity.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn cancel_active(&mut self, key: ActiveJobKey) -> Result<(), WorkerProtocolError> {
        let Some(active) = self.active.get(&key) else {
            return Err(WorkerProtocolError::Rejected(
                "cancellation does not match an active worker attempt".to_owned(),
            ));
        };
        active.handle.cancel();
        Ok(())
    }

    fn admit_cancel_attempt(
        &mut self,
        active_key: ActiveJobKey,
        cancel: CancelAttempt,
    ) -> Result<(), WorkerProtocolError> {
        // The listener has exactly one active stream job. Compare the
        // untrusted routing fields with that retained key before looking up
        // the entry; this stays bounded and prevents a stale token from
        // selecting another attempt that reused the same work identity.
        if cancel.attempt != active_key.attempt || cancel.work_key != active_key.work_key {
            return Err(WorkerProtocolError::Rejected(
                "cancellation does not match an active worker attempt".to_owned(),
            ));
        }
        let expectation = self
            .active
            .get(&active_key)
            .ok_or_else(|| {
                WorkerProtocolError::Rejected(
                    "cancellation does not match an active worker attempt".to_owned(),
                )
            })?
            .cancellation;
        cancel
            .admit_against(expectation, self.limits.transport)
            .map_err(WorkerProtocolError::Replication)?;
        if let Some(active) = self.active.get(&active_key) {
            active.handle.cancel();
        }
        Ok(())
    }

    fn admit_completed_cancel(&mut self, cancel: CancelAttempt) -> Result<(), WorkerProtocolError> {
        let expectation = self.completed_cancellation.ok_or_else(|| {
            WorkerProtocolError::Rejected(
                "cancellation does not match a completed worker attempt".to_owned(),
            )
        })?;
        cancel
            .admit_against(expectation, self.limits.transport)
            .map_err(WorkerProtocolError::Replication)?;
        self.completed_cancellation = None;
        Ok(())
    }

    fn cancel_and_reap(
        &mut self,
        key: ActiveJobKey,
        handle: &CancellationHandle,
        worker: thread::JoinHandle<()>,
    ) -> Result<(), WorkerProtocolError> {
        handle.cancel();
        self.active.remove(&key);
        worker.join().map_err(|_| {
            WorkerProtocolError::Rejected("worker execution thread panicked".to_owned())
        })
    }

    fn begin_job<R: Relation + Send, A: JobAdmission<R>>(
        &mut self,
        request: WireRecipeRequest,
        admission: &mut A,
    ) -> Result<RunningJob, WorkerProtocolError> {
        if !self.is_registered(&request) {
            return Err(WorkerProtocolError::Rejected(
                "recipe is not registered by this worker".to_owned(),
            ));
        }
        let job = admission
            .admit(&request)
            .map_err(|error| WorkerProtocolError::Rejected(error.to_string()))?;
        let handle = job.cancellation.handle().clone();
        let expected_attempt = job.bindings.expected.attempt;
        let expected_work_key = job.bindings.expected.work_key;
        let expected_fence = job.bindings.expected.fence;
        let expected_cancellation = job.bindings.expected.cancellation;
        if expected_attempt != request.attempt
            || backend_engine::WireIdentity::from_typed(&job.bindings.work_key) != request.work_key
            || expected_fence != request.fence
            || expected_cancellation != request.cancellation
        {
            return Err(WorkerProtocolError::Rejected(
                "owner admission did not retain the exact attempt cancellation".to_owned(),
            ));
        }
        let key = ActiveJobKey::from_request(&request);
        if self.active.contains_key(&key) {
            return Err(WorkerProtocolError::Backpressure);
        }
        self.active.insert(
            key,
            ActiveJob {
                handle: handle.clone(),
                cancellation: CancelAttemptExpectation {
                    attempt: expected_attempt,
                    work_key: expected_work_key,
                    cancellation: expected_cancellation,
                    fence: expected_fence,
                },
            },
        );
        let (sender, completed) = mpsc::sync_channel(1);
        let endpoint = Arc::clone(&self.endpoint);
        let worker = match thread::Builder::new()
            .name("backend-worker-job".to_owned())
            .spawn(move || {
                let result = execute_job(endpoint.as_ref(), request, job);
                let _ = sender.send(CompletedJob { key, result });
            }) {
            Ok(worker) => worker,
            Err(error) => {
                self.active.remove(&key);
                return Err(WorkerProtocolError::Io(error.kind()));
            }
        };
        Ok(RunningJob {
            key,
            handle,
            worker,
            completed,
        })
    }

    fn finish_job(
        &mut self,
        running: RunningJob,
    ) -> Result<backend_engine::WireRecipeResult, WorkerProtocolError> {
        let Ok(completed) = running.completed.recv() else {
            let _ = self.cancel_and_reap(running.key, &running.handle, running.worker);
            return Err(WorkerProtocolError::Rejected(
                "worker execution ended without a result".to_owned(),
            ));
        };
        let cancellation = self
            .active
            .remove(&running.key)
            .ok_or_else(|| {
                WorkerProtocolError::Rejected(
                    "worker completion did not match an active attempt".to_owned(),
                )
            })?
            .cancellation;
        if running.worker.join().is_err() {
            return Err(WorkerProtocolError::Rejected(
                "worker execution thread panicked".to_owned(),
            ));
        }
        if completed.key != running.key {
            return Err(WorkerProtocolError::Rejected(
                "worker completion key did not match active attempt".to_owned(),
            ));
        }
        let result = completed
            .result
            .map_err(|error| WorkerProtocolError::Rejected(error.to_string()))?;
        self.completed_cancellation = Some(cancellation);
        Ok(result)
    }

    /// Handles one canonical wire message. Capabilities negotiate and return
    /// the worker's manifest; recipe requests require an owner admission
    /// callback; all other message classes are rejected explicitly.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn handle_message<R: Relation + Send, A: JobAdmission<R>>(
        &mut self,
        message: TransportMessage,
        admission: &mut A,
    ) -> Result<TransportMessage, WorkerProtocolError> {
        if self.closed {
            return Err(WorkerProtocolError::Closed);
        }
        match message {
            TransportMessage::Capabilities(peer) => {
                peer.validate(self.limits.transport)
                    .map_err(WorkerProtocolError::Replication)?;
                self.manifest
                    .negotiate(&peer, self.limits.transport)
                    .map_err(WorkerProtocolError::Replication)?;
                Ok(TransportMessage::Capabilities(self.manifest.clone()))
            }
            TransportMessage::WireRecipeRequest(request) => {
                let running = self.begin_job(request, admission)?;
                let result = self.finish_job(running)?;
                Ok(TransportMessage::WireRecipeResult(Box::new(result)))
            }
            TransportMessage::WireRootSummary(summary) => admission
                .admit_control(TransportMessage::WireRootSummary(summary))?
                .ok_or_else(|| {
                    WorkerProtocolError::Rejected(
                        "worker root-summary admission is not configured".to_owned(),
                    )
                }),
            TransportMessage::Chunk(frame) => admission
                .admit_control(TransportMessage::Chunk(frame))?
                .ok_or_else(|| {
                    WorkerProtocolError::Rejected(
                        "worker chunk admission does not produce a response".to_owned(),
                    )
                }),
            TransportMessage::ClosurePageRequest(request) => {
                admission.admit_page(request)?.ok_or_else(|| {
                    WorkerProtocolError::Rejected(
                        "worker Merkle page admission is not configured".to_owned(),
                    )
                })
            }
            TransportMessage::ClosurePageResponse(response) => {
                admission.admit_page_response(response)?.ok_or_else(|| {
                    WorkerProtocolError::Rejected(
                        "worker Merkle page response admission is not configured".to_owned(),
                    )
                })
            }
            TransportMessage::ClosureRootOffer(offer) => admission
                .admit_control(TransportMessage::ClosureRootOffer(offer))?
                .ok_or_else(|| {
                    WorkerProtocolError::Rejected(
                        "worker closure-root admission is not configured".to_owned(),
                    )
                }),
            TransportMessage::ClosureNeedRequest(request) => admission
                .admit_control(TransportMessage::ClosureNeedRequest(request))?
                .ok_or_else(|| {
                    WorkerProtocolError::Rejected(
                        "worker closure-need admission is not configured".to_owned(),
                    )
                }),
            TransportMessage::WireRecipeResult(_) => Err(WorkerProtocolError::Rejected(
                "worker does not accept recipe results".to_owned(),
            )),
            _ => Err(WorkerProtocolError::Rejected(
                "message class is not executable by a pure worker".to_owned(),
            )),
        }
    }

    /// Marks the endpoint closed. In-flight pure work is fenced by its
    /// cancellation observation supplied in [`WorkerJob`].
    pub fn close(&mut self) {
        self.closed = true;
    }

    /// Returns whether the worker is closed.
    #[must_use]
    pub const fn is_closed(&self) -> bool {
        self.closed
    }
}

#[cfg(test)]
#[path = "service/tests.rs"]
mod tests;
