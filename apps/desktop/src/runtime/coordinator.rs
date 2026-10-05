//! UI-thread runtime coordinator.

use super::actor::{CancellationToken, EngineActor, EngineRequest, LocalRead};
use super::mailbox::{CoalesceKey, Coalescible};
use super::mapping::{MappingError, map_event};
use crate::core::{FaultCode, LocalProjectId, SnapshotReadModel};
use crate::model::AppSnapshot;
use crate::navigation::{Effect, EngineCommand, Intent, RequestId, reduce};
use std::collections::BTreeMap;
use std::sync::Arc;

struct InflightRequest {
    basis: crate::core::VersionedRoot,
    cancel: CancellationToken,
    lane: Option<CoalesceKey>,
    index_project: Option<LocalProjectId>,
    /// Whether the result is producer-root state that a newer root makes
    /// stale. Local manifest reads are not.
    rooted: bool,
    /// A connection check may only succeed from its own revision DTO.
    connection_probe: bool,
}

/// Runtime events observed by the UI entity graph.
#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)] // short-lived, drained once per wake
pub enum RuntimeEvent {
    /// A new immutable snapshot was accepted.
    SnapshotChanged(Arc<AppSnapshot>),
    /// A result was rejected because a newer root/request won.
    RejectedStale(MappingError),
    /// Persistence was requested by the reducer.
    PersistRequested(Arc<AppSnapshot>),
    /// One request reached exactly one terminal outcome, including requests
    /// cancelled or refused before the actor could answer.
    RequestCompleted {
        /// Request identity allocated by the runtime.
        request: RequestId,
        /// The terminal outcome; cancellation and refusal never imply success.
        outcome: RequestOutcome,
    },
}

/// Closed terminal state for one request lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestOutcome {
    /// The producer response was admitted into the snapshot.
    Succeeded,
    /// The producer answered with an error or its response failed admission.
    Failed(FaultCode),
    /// Work that had been accepted was stopped without claiming a result.
    Cancelled,
    /// A newer request made this request's result irrelevant.
    Superseded,
    /// Work could not be admitted to the actor's bounded request queue.
    Refused(RequestRefusalReason),
}

/// Why a request could not be admitted to the actor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestRefusalReason {
    /// The bounded actor queue had no free slot.
    QueueFull,
    /// The actor's request channel had already closed.
    Closed,
}

/// The only state owner on the UI thread. Engine work is submitted and polled
/// through a dedicated actor; selectors and widgets observe its snapshot.
pub struct DesktopRuntime {
    snapshot: Arc<AppSnapshot>,
    actor: EngineActor,
    inflight: BTreeMap<RequestId, InflightRequest>,
    next_request: u64,
}

impl std::fmt::Debug for DesktopRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DesktopRuntime")
            .field("root", &self.snapshot.key())
            .field("inflight", &self.inflight.len())
            .finish_non_exhaustive()
    }
}

impl DesktopRuntime {
    /// Creates a runtime around one admitted snapshot and actor.
    #[must_use]
    pub fn new(snapshot: AppSnapshot, actor: EngineActor) -> Self {
        Self {
            snapshot: Arc::new(snapshot),
            actor,
            inflight: BTreeMap::new(),
            next_request: 1,
        }
    }

    /// Returns the immutable snapshot shared with views/selectors.
    #[must_use]
    pub fn snapshot(&self) -> Arc<AppSnapshot> {
        Arc::clone(&self.snapshot)
    }

    /// The bootstrap host admits local state once before the first serving
    /// root. This grants no producer authority and submits no engine work.
    pub(crate) fn admit_bootstrap(&mut self, bound: &crate::host::bootstrap::BoundWorkspace, origin: &AppSnapshot) {
        self.snapshot = Arc::new(bound.admit_snapshot(&self.snapshot, origin));
    }

    /// A synchronous local preflight refusal, callable only inside the runtime
    /// boundary. It cannot be queued as an unrestricted late UI intent.
    pub(crate) fn reject_unsent_index(&mut self, project: &LocalProjectId, basis: crate::core::VersionedRoot, operation: Option<&crate::model::IndexOperationClaim>, message: Arc<str>) -> Vec<RuntimeEvent> {
        if !self.snapshot.key().same_authority(basis) { return Vec::new(); }
        let mut workspace = self.snapshot.workspace().clone();
        let mut projects = workspace.projects.to_vec();
        let Some(row) = projects.iter_mut().find(|row| row.id == *project
            && row.phase == crate::model::ProjectPhase::Indexing && row.request.is_none()
            && row.operation.as_ref().is_none_or(|saved| operation.is_some_and(|operation| saved.same_request(operation) && saved.observation.is_none()))) else { return Vec::new(); };
        row.phase = crate::model::ProjectPhase::Failed;
        row.error = Some(message);
        row.progress = None;
        row.files_indexed = None;
        row.operation = None; // this boundary proves no mutation was sent
        workspace.projects = projects.into();
        self.snapshot = Arc::new(self.snapshot.with_workspace(workspace));
        vec![RuntimeEvent::SnapshotChanged(self.snapshot.clone()), RuntimeEvent::PersistRequested(self.snapshot.clone())]
    }

    /// A local writer diagnostic is ephemeral; it never changes producer
    /// authority, operation evidence, or the durable projection itself.
    pub(crate) fn set_persistence_fault(&mut self, why: Option<Arc<str>>) {
        let mut workspace = self.snapshot.workspace().clone();
        let mut notes = workspace.notes.iter().filter(|note| !matches!(note, crate::model::Note::StateNotSaved { .. })).cloned().collect::<Vec<_>>();
        if let Some(why) = why { notes.push(crate::model::Note::StateNotSaved { why }); }
        workspace.notes = notes.into();
        self.snapshot = Arc::new(self.snapshot.with_workspace(workspace));
    }

    /// Returns the next request identity without touching the engine.
    pub fn allocate_request(&mut self) -> RequestId {
        let request = RequestId::from_authority(self.snapshot.key(), self.next_request);
        self.next_request = self.next_request.wrapping_add(1).max(1);
        request
    }

    /// Applies a typed intent and submits only the effects it emits.
    pub fn dispatch(&mut self, intent: Intent) -> Vec<RuntimeEvent> {
        #[cfg(any(test, feature = "visual-harness"))]
        let owner = super::actor::IndexMutationLease::capture(None, None);
        #[cfg(not(any(test, feature = "visual-harness")))]
        let owner = None;
        self.dispatch_with_owner(intent, owner)
    }

    /// Only the root's checked durable acknowledgment may use this boundary.
    pub(crate) fn dispatch_saved_index(&mut self, intent: Intent, owner: super::actor::IndexMutationLease) -> Vec<RuntimeEvent> {
        self.release_unsent_claim(&intent);
        self.dispatch_with_owner(intent, Some(owner))
    }

    /// A root-owned preflight proves this exact claim has never reached transport.
    pub(crate) fn release_unsent_claim(&mut self, intent: &Intent) {
        let Intent::IndexProject { project, operation, .. } = intent else { return; };
        let mut workspace = self.snapshot.workspace().clone();
        let mut projects = workspace.projects.to_vec();
        if let Some(row) = projects.iter_mut().find(|row| row.id == *project
            && row.phase == crate::model::ProjectPhase::Indexing && row.request.is_none()
            && row.operation.as_ref().is_some_and(|saved| saved.same_request(operation) && saved.observation.is_none())) {
            row.operation = None;
            workspace.projects = projects.into();
            self.snapshot = Arc::new(self.snapshot.with_workspace(workspace));
        }
    }

    fn dispatch_with_owner(&mut self, intent: Intent, owner: Option<super::actor::IndexMutationLease>) -> Vec<RuntimeEvent> {
        let reduction = reduce(&self.snapshot, intent);
        self.snapshot = Arc::new(reduction.snapshot);
        let mut events = vec![RuntimeEvent::SnapshotChanged(Arc::clone(&self.snapshot))];
        for effect in reduction.effects {
            match effect {
                Effect::Engine(command) => events.extend(self.submit(command, owner.clone())),
                Effect::Persist => {
                    events.push(RuntimeEvent::PersistRequested(Arc::clone(&self.snapshot)));
                }
                Effect::Cancel(request) => events.extend(self.cancel(request)),
                Effect::CancelAll => events.extend(self.cancel_all()),
            }
        }
        events
    }

    #[allow(clippy::too_many_lines)] // one arm per engine command, kept flat
    fn submit(&mut self, command: EngineCommand, owner: Option<super::actor::IndexMutationLease>) -> Vec<RuntimeEvent> {
        let (request, engine_request, basis, cancel) = match command {
            EngineCommand::CheckConnection { basis, request } => {
                let cancel = CancellationToken::new();
                (request, EngineRequest::ConnectionProbe { request, basis, cancel: cancel.clone() }, basis, cancel)
            }
            EngineCommand::ReadLocalPackage {
                project,
                basis,
                request,
            } => {
                return self.submit_local(LocalRead {
                    request,
                    project,
                    basis,
                    cancel: CancellationToken::new(),
                });
            }
            EngineCommand::ReadRoot { basis, request } => {
                let cancel = CancellationToken::new();
                (
                    request,
                    EngineRequest::Root {
                        request,
                        basis,
                        cancel: cancel.clone(),
                    },
                    basis,
                    cancel,
                )
            }
            EngineCommand::IndexProject {
                project,
                operation,
                basis,
                request,
            } => {
                let cancel = CancellationToken::new();
                (
                    request,
                    EngineRequest::IndexProject {
                        owner,
                        request,
                        project,
                        operation,
                        basis,
                        cancel: cancel.clone(),
                    },
                    basis,
                    cancel,
                )
            }
            EngineCommand::IndexOperationStatus { project, operation, basis, request } => {
                let cancel = CancellationToken::new();
                (request, EngineRequest::IndexOperationStatus {
                    request, project, operation, basis, cancel: cancel.clone(),
                }, basis, cancel)
            }
            EngineCommand::ReadObject {
                object,
                delta,
                basis,
                request,
            } => {
                let cancel = CancellationToken::new();
                (
                    request,
                    EngineRequest::Object {
                        request,
                        object,
                        delta,
                        basis,
                        cancel: cancel.clone(),
                    },
                    basis,
                    cancel,
                )
            }
            EngineCommand::RefreshSurface {
                command,
                basis,
                request,
            } => {
                let cancel = CancellationToken::new();
                (
                    request,
                    EngineRequest::Surface {
                        request,
                        command,
                        basis,
                        cancel: cancel.clone(),
                    },
                    basis,
                    cancel,
                )
            }
        };
        let lane = engine_request.coalesce_key();
        let mut events = self.retire_lane(request, lane, RequestOutcome::Superseded);
        self.inflight.insert(
            request,
            InflightRequest {
                basis,
                cancel,
                lane,
                index_project: match &engine_request {
                    EngineRequest::IndexProject { project, .. } | EngineRequest::IndexOperationStatus { project, .. } => Some(project.clone()),
                    _ => None,
                },
                rooted: true,
                connection_probe: matches!(&engine_request, EngineRequest::ConnectionProbe { .. }),
            },
        );
        match self.actor.try_submit_coalesced(engine_request) {
            super::mailbox::PushResult::Enqueued => {}
            super::mailbox::PushResult::Coalesced(old) => {
                if let Some(event) =
                    self.retire_request(old.request(), RequestOutcome::Superseded)
                {
                    events.push(event);
                }
            }
            super::mailbox::PushResult::Full(request) => {
                events.extend(self.hold_refused_index(&request));
                if let Some(event) = self.retire_request(
                    request.request(),
                    RequestOutcome::Refused(RequestRefusalReason::QueueFull),
                ) {
                    events.push(event);
                }
            }
            super::mailbox::PushResult::Closed(request) => {
                events.extend(self.hold_refused_index(&request));
                if let Some(event) = self.retire_request(
                    request.request(),
                    RequestOutcome::Refused(RequestRefusalReason::Closed),
                ) {
                    events.push(event);
                }
            }
        }
        events
    }

    /// A refused first-send request never entered the adapter and can release
    /// its claim. A refused status read says nothing about already accepted
    /// work, so that request retains its exact durable claim for reconciliation.
    fn hold_refused_index(&mut self, request: &EngineRequest) -> Vec<RuntimeEvent> {
        let (EngineRequest::IndexProject { project, .. } | EngineRequest::IndexOperationStatus { project, .. }) = request else { return Vec::new(); };
        let fault = if matches!(request, EngineRequest::IndexProject { .. }) {
            super::actor::EngineFault::IndexNotSent { project: project.clone(), error: crate::core::ErrorValue::new(
                FaultCode::Transport, "The local index queue did not accept this request. Nothing was sent; try again after it settles.") }
        } else { super::actor::EngineFault::IndexUnconfirmed { project: project.clone() } };
        let event = super::actor::EngineEvent { basis: self.inflight.get(&request.request()).map_or(self.snapshot.key(), |entry| entry.basis), request: request.request(), lane: None,
            result: Err(fault) };
        let Ok(snapshot) = map_event(&self.snapshot, event) else { return Vec::new(); };
        self.snapshot = Arc::new(snapshot);
        vec![RuntimeEvent::SnapshotChanged(self.snapshot.clone()), RuntimeEvent::PersistRequested(self.snapshot.clone())]
    }

    /// Submits one local read on the actor's local lane, replacing any
    /// older read for the same project.
    fn submit_local(&mut self, read: LocalRead) -> Vec<RuntimeEvent> {
        let request = read.request;
        let lane = read.coalesce_key();
        let mut events = self.retire_lane(request, lane, RequestOutcome::Superseded);
        self.inflight.insert(
            request,
            InflightRequest {
                basis: read.basis,
                cancel: read.cancel.clone(),
                lane,
                index_project: None,
                rooted: false,
                connection_probe: false,
            },
        );
        match self.actor.try_submit_local(read) {
            super::mailbox::PushResult::Enqueued => {}
            super::mailbox::PushResult::Coalesced(old) => {
                if let Some(event) = self.retire_request(old.request, RequestOutcome::Superseded) {
                    events.push(event);
                }
            }
            super::mailbox::PushResult::Full(read) => {
                if let Some(event) = self.retire_request(
                    read.request,
                    RequestOutcome::Refused(RequestRefusalReason::QueueFull),
                ) {
                    events.push(event);
                }
            }
            super::mailbox::PushResult::Closed(read) => {
                if let Some(event) = self.retire_request(
                    read.request,
                    RequestOutcome::Refused(RequestRefusalReason::Closed),
                ) {
                    events.push(event);
                }
            }
        }
        events
    }

    fn cancel(&mut self, request: RequestId) -> Vec<RuntimeEvent> {
        if let Some((cancel, is_index)) = self
            .inflight
            .get(&request)
            .map(|old| (old.cancel.clone(), old.index_project.is_some()))
        {
            cancel.cancel();
            if !is_index {
                return self
                    .retire_request(request, RequestOutcome::Cancelled)
                    .into_iter()
                    .collect();
            }
        }
        Vec::new()
    }

    fn cancel_all(&mut self) -> Vec<RuntimeEvent> {
        let inflight = std::mem::take(&mut self.inflight);
        let mut events = Vec::new();
        for (id, request) in inflight {
            request.cancel.cancel();
            if request.index_project.is_some() {
                self.inflight.insert(id, request);
            } else {
                events.push(RuntimeEvent::RequestCompleted {
                    request: id,
                    outcome: RequestOutcome::Cancelled,
                });
            }
        }
        events
    }

    /// Polls actor events without waiting on the UI thread.
    pub fn poll(&mut self) -> Vec<RuntimeEvent> {
        let mut events = Vec::new();
        for event in self.actor.drain_events() {
            let request = event.request;
            let Some(inflight) = self.inflight.remove(&request) else {
                events.push(RuntimeEvent::RejectedStale(MappingError::Engine(
                    super::actor::EngineFault::Superseded,
                )));
                continue;
            };
            let index_request = inflight.index_project.is_some();
            if !event.basis.same_authority(inflight.basis)
                || (!index_request
                    && inflight.rooted
                    && !event.basis.same_authority(self.snapshot.key()))
            {
                events.push(RuntimeEvent::RejectedStale(MappingError::StaleRoot {
                    expected: self.snapshot.key(),
                    observed: event.basis,
                }));
                events.push(RuntimeEvent::RequestCompleted {
                    request,
                    outcome: RequestOutcome::Superseded,
                });
                continue;
            }
            if inflight.connection_probe
                && matches!(&event.result, Ok(dto) if !matches!(dto, super::actor::EngineDto::ConnectionProbe { .. }))
            {
                events.push(RuntimeEvent::RejectedStale(MappingError::Engine(
                    super::actor::EngineFault::Failed(crate::core::ErrorValue::new(
                        FaultCode::Protocol, "connection check returned a non-revision reply",
                    )),
                )));
                events.push(RuntimeEvent::RequestCompleted {
                    request,
                    outcome: RequestOutcome::Failed(FaultCode::Protocol),
                });
                continue;
            }
            let actor_outcome = match &event.result {
                Err(
                    super::actor::EngineFault::Cancelled
                    | super::actor::EngineFault::IndexCancelled { .. },
                ) => RequestOutcome::Cancelled,
                Err(super::actor::EngineFault::Superseded) => RequestOutcome::Superseded,
                Err(super::actor::EngineFault::Failed(error) | super::actor::EngineFault::IndexFailed { error, .. } | super::actor::EngineFault::IndexNotSent { error, .. }) => RequestOutcome::Failed(error.code()),
                Err(_) => RequestOutcome::Failed(FaultCode::Protocol),
                Ok(_) => RequestOutcome::Succeeded,
            };
            events.extend(self.retire_lane(request, inflight.lane, RequestOutcome::Superseded));
            match map_event(&self.snapshot, event) {
                Ok(snapshot) => {
                    // An index that finished, failed or was stopped changes what
                    // the shelf says about a project; a relaunch must find it as
                    // it was left, not wait for the next intent to write it.
                    let shelf_moved = snapshot.workspace().projects != self.snapshot.workspace().projects;
                    self.snapshot = Arc::new(snapshot);
                    events.push(RuntimeEvent::SnapshotChanged(Arc::clone(&self.snapshot)));
                    if shelf_moved {
                        events.push(RuntimeEvent::PersistRequested(Arc::clone(&self.snapshot)));
                    }
                    events.push(RuntimeEvent::RequestCompleted {
                        request,
                        outcome: actor_outcome,
                    });
                }
                Err(error) => {
                    let outcome = match &error {
                        MappingError::StaleRoot { .. }
                        | MappingError::Engine(super::actor::EngineFault::Superseded) => {
                            RequestOutcome::Superseded
                        }
                        MappingError::Engine(
                            super::actor::EngineFault::Cancelled
                            | super::actor::EngineFault::IndexCancelled { .. },
                        ) => RequestOutcome::Cancelled,
                        MappingError::Engine(super::actor::EngineFault::Failed(error)
                            | super::actor::EngineFault::IndexFailed { error, .. }
                            | super::actor::EngineFault::IndexNotSent { error, .. }) => RequestOutcome::Failed(error.code()),
                        MappingError::Engine(super::actor::EngineFault::IndexUnconfirmed { .. }
                            | super::actor::EngineFault::MutationUnconfirmed) => RequestOutcome::Failed(FaultCode::Transport),
                        MappingError::BasisMismatch { .. }
                        | MappingError::RequestMismatch { .. } => RequestOutcome::Failed(FaultCode::Protocol),
                    };
                    events.push(RuntimeEvent::RejectedStale(error));
                    events.push(RuntimeEvent::RequestCompleted { request, outcome });
                }
            }
        }
        events
    }

    /// Returns whether a request is still current.
    #[must_use]
    pub fn is_inflight(&self, request: RequestId) -> bool {
        self.inflight.contains_key(&request)
    }

    /// Returns the bounded number of actor results waiting for the next frame.
    #[must_use]
    pub fn queued_results(&self) -> usize {
        self.actor.queued_events()
    }

    /// Returns whether any engine work is in flight or waiting to be drained.
    ///
    /// Diagnostic only: the UI never requests frames on this. Results wake
    /// the owner through [`Self::take_wake`], so an idle window with a
    /// long-running request schedules no frame at all.
    #[must_use]
    pub fn has_pending_work(&self) -> bool {
        !self.inflight.is_empty() || self.actor.queued_events() != 0
    }

    /// [`Self::has_pending_work`] apart from indexing, which is one owner
    /// call that runs for minutes: the journey harness waits for everything
    /// else a frame shows and awaits indexing as its own step.
    #[must_use]
    pub fn has_pending_work_besides_indexing(&self) -> bool {
        self.inflight.values().any(|request| request.index_project.is_none()) || self.actor.queued_events() != 0
    }

    /// Takes the wake signal the actor raises after delivering each result.
    /// The owning entity awaits it in one task and calls [`Self::poll`].
    pub fn take_wake(&mut self) -> Option<super::wake::WakeReceiver> {
        self.actor.take_wake()
    }

    fn retire_lane(
        &mut self,
        request: RequestId,
        lane: Option<CoalesceKey>,
        outcome: RequestOutcome,
    ) -> Vec<RuntimeEvent> {
        let Some(lane) = lane else {
            return Vec::new();
        };
        let superseded = self
            .inflight
            .iter()
            .filter_map(|(id, candidate)| {
                (*id != request && candidate.lane == Some(lane)).then_some(*id)
            })
            .collect::<Vec<_>>();
        let mut events = Vec::with_capacity(superseded.len());
        for id in superseded {
            if let Some(event) = self.retire_request(id, outcome) {
                events.push(event);
            }
        }
        events
    }

    fn retire_request(
        &mut self,
        request: RequestId,
        outcome: RequestOutcome,
    ) -> Option<RuntimeEvent> {
        let inflight = self.inflight.remove(&request)?;
        inflight.cancel.cancel();
        Some(RuntimeEvent::RequestCompleted { request, outcome })
    }
}

impl SnapshotReadModel for DesktopRuntime {
    fn snapshot(&self) -> Arc<AppSnapshot> {
        self.snapshot()
    }
}
