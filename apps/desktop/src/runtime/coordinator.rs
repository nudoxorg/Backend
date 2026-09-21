//! UI-thread runtime coordinator.

use super::actor::{CancellationToken, EngineActor, EngineRequest};
use super::mailbox::{CoalesceKey, Coalescible};
use super::mapping::{MappingError, map_event};
use crate::core::SnapshotReadModel;
use crate::model::AppSnapshot;
use crate::navigation::{Effect, EngineCommand, Intent, RequestId, reduce};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Runtime events observed by the UI entity graph.
#[derive(Clone, Debug)]
pub enum RuntimeEvent {
    /// A new immutable snapshot was accepted.
    SnapshotChanged(Arc<AppSnapshot>),
    /// A result was rejected because a newer root/request won.
    RejectedStale(MappingError),
    /// Persistence was requested by the reducer.
    PersistRequested(Arc<AppSnapshot>),
}

/// The only state owner on the UI thread. Engine work is submitted and polled
/// through a dedicated actor; selectors and widgets observe its snapshot.
pub struct DesktopRuntime {
    snapshot: Arc<AppSnapshot>,
    actor: EngineActor,
    inflight: BTreeMap<
        RequestId,
        (
            crate::core::VersionedRoot,
            CancellationToken,
            Option<CoalesceKey>,
        ),
    >,
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
        let request = RequestId::new(self.next_request);
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
                Effect::Cancel(request) => self.cancel(request),
                Effect::CancelAll => self.cancel_all(),
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
        let older = self
            .inflight
            .iter()
            .filter_map(|(id, (old_basis, token, _))| {
                old_basis
                    .is_older_authority(basis)
                    .then_some((*id, token.clone()))
            })
            .collect::<Vec<_>>();
        for (id, token) in older {
            token.cancel();
            self.inflight.remove(&id);
        }
        self.inflight.insert(request, (basis, cancel, lane));
        match self.actor.try_submit_coalesced(engine_request) {
            super::mailbox::PushResult::Enqueued => {}
            super::mailbox::PushResult::Coalesced(old) => {
                if let Some((_, token, _)) = self.inflight.remove(&old.request()) {
                    token.cancel();
                }
            }
            super::mailbox::PushResult::Full(request)
            | super::mailbox::PushResult::Closed(request) => {
                if let Some((_, token, _)) = self.inflight.remove(&request.request()) {
                    token.cancel();
                }
            }
        }
    }

    fn cancel(&mut self, request: RequestId) {
        if let Some((_, token, _)) = self.inflight.remove(&request) {
            token.cancel();
        }
    }

    fn cancel_all(&mut self) {
        let inflight = std::mem::take(&mut self.inflight);
        for (_, (_, token, _)) in inflight {
            token.cancel();
        }
    }

    /// Polls actor events without waiting on the UI thread.
    pub fn poll(&mut self) -> Vec<RuntimeEvent> {
        let mut events = Vec::new();
        for event in self.actor.drain_events() {
            let request = event.request;
            let Some((basis, _, lane)) = self.inflight.remove(&request) else {
                events.push(RuntimeEvent::RejectedStale(MappingError::Engine(
                    super::actor::EngineFault::Superseded,
                )));
                continue;
            };
            if !event.basis.same_authority(basis)
                || !event.basis.same_authority(self.snapshot.key())
            {
                events.push(RuntimeEvent::RejectedStale(MappingError::StaleRoot {
                    expected: self.snapshot.key(),
                    observed: event.basis,
                }));
                continue;
            }
            self.retire_lane(request, lane);
            match map_event(&self.snapshot, event) {
                Ok(snapshot) => {
                    self.snapshot = Arc::new(snapshot);
                    events.push(RuntimeEvent::SnapshotChanged(Arc::clone(&self.snapshot)));
                }
                Err(error) => events.push(RuntimeEvent::RejectedStale(error)),
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

    fn retire_lane(&mut self, request: RequestId, lane: Option<CoalesceKey>) {
        let Some(lane) = lane else {
            return;
        };
        let superseded = self
            .inflight
            .iter()
            .filter_map(|(id, (_, token, candidate))| {
                (*id != request && *candidate == Some(lane)).then_some((*id, token.clone()))
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
