//! UI-thread runtime coordinator.

use super::actor::{CancellationToken, EngineActor, EngineRequest};
use super::mailbox::{CoalesceKey, Coalescible};
use super::mapping::{MappingError, map_event};
use crate::core::{LocalProjectId, SnapshotReadModel};
use crate::model::AppSnapshot;
use crate::navigation::{Effect, EngineCommand, Intent, RequestId, reduce};
use std::collections::BTreeMap;
use std::sync::Arc;

struct InflightRequest {
    basis: crate::core::VersionedRoot,
    cancel: CancellationToken,
    lane: Option<CoalesceKey>,
    index_project: Option<LocalProjectId>,
}

/// Runtime events observed by the UI entity graph.
#[derive(Clone, Debug)]
pub enum RuntimeEvent {
    /// A new immutable snapshot was accepted.
    SnapshotChanged(Arc<AppSnapshot>),
    /// A result was rejected because a newer root/request won.
    RejectedStale(MappingError),
    /// Persistence was requested by the reducer.
    PersistRequested(Arc<AppSnapshot>),
    /// One actor request reached a terminal mapping outcome. The UI uses this
    /// only for the connection probe; ordinary reads remain snapshot-driven.
    RequestCompleted {
        /// Request identity allocated by the runtime.
        request: RequestId,
        /// Whether the response was admitted as a typed snapshot update.
        succeeded: bool,
    },
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

    /// Returns the next request identity without touching the engine.
    pub fn allocate_request(&mut self) -> RequestId {
        let request = RequestId::from_authority(self.snapshot.key(), self.next_request);
        self.next_request = self.next_request.wrapping_add(1).max(1);
        request
    }

    /// Applies a typed intent and submits only the effects it emits.
    pub fn dispatch(&mut self, intent: Intent) -> Vec<RuntimeEvent> {
        let reduction = reduce(&self.snapshot, intent);
        self.snapshot = Arc::new(reduction.snapshot);
        let mut events = vec![RuntimeEvent::SnapshotChanged(Arc::clone(&self.snapshot))];
        for effect in reduction.effects {
            match effect {
                Effect::Engine(command) => self.submit(command),
                Effect::Persist => {
                    events.push(RuntimeEvent::PersistRequested(Arc::clone(&self.snapshot)))
                }
                Effect::Cancel(request) => events.extend(self.cancel(request)),
                Effect::CancelAll => events.extend(self.cancel_all()),
            }
        }
        events
    }

    fn submit(&mut self, command: EngineCommand) {
        let (request, engine_request, basis, cancel) = match command {
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
                basis,
                request,
            } => {
                let cancel = CancellationToken::new();
                (
                    request,
                    EngineRequest::IndexProject {
                        request,
                        project,
                        basis,
                        cancel: cancel.clone(),
                    },
                    basis,
                    cancel,
                )
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
        let same_lane = self
            .inflight
            .iter()
            .filter_map(|(id, old)| {
                (lane.is_some() && old.lane == lane && *id != request)
                    .then_some((*id, old.cancel.clone()))
            })
            .collect::<Vec<_>>();
        for (id, token) in same_lane {
            token.cancel();
            self.inflight.remove(&id);
        }
        self.inflight.insert(
            request,
            InflightRequest {
                basis,
                cancel,
                lane,
                index_project: match &engine_request {
                    EngineRequest::IndexProject { project, .. } => Some(project.clone()),
                    _ => None,
                },
            },
        );
        match self.actor.try_submit_coalesced(engine_request) {
            super::mailbox::PushResult::Enqueued => {}
            super::mailbox::PushResult::Coalesced(old) => {
                if let Some(old) = self.inflight.remove(&old.request()) {
                    old.cancel.cancel();
                }
            }
            super::mailbox::PushResult::Full(request)
            | super::mailbox::PushResult::Closed(request) => {
                if let Some(old) = self.inflight.remove(&request.request()) {
                    old.cancel.cancel();
                }
            }
        }
    }

    fn cancel(&mut self, request: RequestId) -> Vec<RuntimeEvent> {
        if let Some((cancel, is_index)) = self
            .inflight
            .get(&request)
            .map(|old| (old.cancel.clone(), old.index_project.is_some()))
        {
            cancel.cancel();
            if !is_index {
                self.inflight.remove(&request);
            }
        }
        Vec::new()
    }

    fn cancel_all(&mut self) -> Vec<RuntimeEvent> {
        let inflight = std::mem::take(&mut self.inflight);
        for (id, request) in inflight {
            request.cancel.cancel();
            if request.index_project.is_some() {
                self.inflight.insert(id, request);
            }
        }
        Vec::new()
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
                || (!index_request && !event.basis.same_authority(self.snapshot.key()))
            {
                events.push(RuntimeEvent::RejectedStale(MappingError::StaleRoot {
                    expected: self.snapshot.key(),
                    observed: event.basis,
                }));
                events.push(RuntimeEvent::RequestCompleted {
                    request,
                    succeeded: false,
                });
                continue;
            }
            self.retire_lane(request, inflight.lane);
            match map_event(&self.snapshot, event) {
                Ok(snapshot) => {
                    self.snapshot = Arc::new(snapshot);
                    events.push(RuntimeEvent::SnapshotChanged(Arc::clone(&self.snapshot)));
                    events.push(RuntimeEvent::RequestCompleted {
                        request,
                        succeeded: true,
                    });
                }
                Err(error) => {
                    events.push(RuntimeEvent::RejectedStale(error));
                    events.push(RuntimeEvent::RequestCompleted {
                        request,
                        succeeded: false,
                    });
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

    /// Returns whether the UI should keep a frame budget alive.
    ///
    /// Engine responses are delivered through the same GPUI event loop as
    /// animation frames. The shell requests frames while a request is
    /// in-flight, then stops as soon as the queue and request set are empty.
    #[must_use]
    pub fn needs_frame(&self) -> bool {
        !self.inflight.is_empty() || self.actor.queued_events() != 0
    }

    fn retire_lane(&mut self, request: RequestId, lane: Option<CoalesceKey>) {
        let Some(lane) = lane else {
            return;
        };
        let superseded = self
            .inflight
            .iter()
            .filter_map(|(id, candidate)| {
                (*id != request && candidate.lane == Some(lane))
                    .then_some((*id, candidate.cancel.clone()))
            })
            .collect::<Vec<_>>();
        for (id, token) in superseded {
            token.cancel();
            self.inflight.remove(&id);
        }
    }
}

impl SnapshotReadModel for DesktopRuntime {
    fn snapshot(&self) -> Arc<AppSnapshot> {
        self.snapshot()
    }
}
