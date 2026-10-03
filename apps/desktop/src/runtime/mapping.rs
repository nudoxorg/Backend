//! Engine DTO → immutable snapshot mapping.

use super::actor::{EngineDto, EngineEvent, EngineFault};
use crate::core::{Resource, VersionedRoot};
use crate::model::{AppSnapshot, CatalogState, ProjectPhase};
use crate::navigation::RequestId;
use std::sync::Arc;

/// Mapping failures remain typed until the accessibility/presentation edge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MappingError {
    /// A producer root older than the current snapshot arrived.
    StaleRoot {
        /// Current UI authority.
        expected: VersionedRoot,
        /// Result authority.
        observed: VersionedRoot,
    },
    /// The DTO did not describe the current request basis.
    BasisMismatch {
        /// Request authority.
        expected: VersionedRoot,
        /// DTO authority.
        observed: VersionedRoot,
    },
    /// The DTO's local request identity did not match its event envelope.
    RequestMismatch {
        /// Request identity tracked by the coordinator.
        expected: RequestId,
        /// Request identity supplied by the client DTO.
        observed: RequestId,
    },
    /// The actor rejected the request.
    Engine(EngineFault),
}

/// Applies one actor event to a snapshot after authority/staleness checks.
///
/// # Errors
/// Returns a [`MappingError`] when the event is stale, names another basis
/// or request, or carries an engine fault the snapshot does not represent.
#[allow(clippy::too_many_lines, clippy::result_large_err)] // one arm per DTO; errors are drained once
pub fn map_event(current: &AppSnapshot, event: EngineEvent) -> Result<AppSnapshot, MappingError> {
    let EngineEvent {
        basis,
        request: event_request,
        result,
        ..
    } = event;
    let dto = match result {
        Ok(dto) => dto,
        Err(EngineFault::IndexFailed { project, error }) => {
            return Ok(mark_index_failed(
                current,
                event_request,
                &project,
                error.message(),
            ));
        }
        Err(EngineFault::IndexCancelled { project }) => {
            return Ok(mark_index_cancelled(current, event_request, &project));
        }
        Err(EngineFault::IndexUnconfirmed { project }) => {
            return Ok(mark_index_unconfirmed(
                current,
                event_request,
                &project,
                "The owner's index reply was interrupted. This request may still be running or may have completed; its exact operation must be checked before another index starts.",
            ));
        }
        Err(error) => return Err(MappingError::Engine(error)),
    };
    match dto {
        EngineDto::Root {
            request,
            basis: dto_basis,
            key,
            revision,
            delta,
            project,
            catalog,
        } => {
            if request != event_request {
                return Err(MappingError::RequestMismatch {
                    expected: event_request,
                    observed: request,
                });
            }
            if !dto_basis.same_authority(basis) {
                return Err(MappingError::BasisMismatch {
                    expected: basis,
                    observed: dto_basis,
                });
            }
            if key.revision() != revision {
                return Err(MappingError::Engine(EngineFault::Failed(
                    crate::core::ErrorValue::new(
                        crate::core::FaultCode::Protocol,
                        "root DTO generation did not match its service cursor",
                    ),
                )));
            }
            if key.is_older_authority(current.key())
                || (key.same_producer_generation(current.key())
                    && !key.same_authority(current.key()))
            {
                return Err(MappingError::StaleRoot {
                    expected: current.key(),
                    observed: key,
                });
            }
            let snapshot = current.with_key(key, delta);
            let snapshot = project.map_or(snapshot.clone(), |project| {
                snapshot.with_project(project.into_model(), key)
            });
            Ok(catalog.map_or(snapshot.clone(), |packages| {
                snapshot.with_catalog(CatalogState { packages }, key)
            }))
        }
        EngineDto::Object {
            request,
            basis: dto_basis,
            delta,
            ..
        } => {
            if request != event_request {
                return Err(MappingError::RequestMismatch {
                    expected: event_request,
                    observed: request,
                });
            }
            if !dto_basis.same_authority(basis) {
                return Err(MappingError::BasisMismatch {
                    expected: basis,
                    observed: dto_basis,
                });
            }
            // Object reads do not advance the producer root, but their
            // admitted delta is still part of the immutable snapshot key for
            // selector/layout invalidation.
            Ok(current.with_key(current.key(), Some(delta)))
        }
        EngineDto::Surface {
            request,
            basis: dto_basis,
            command: _,
            reply: _,
        } => {
            if request != event_request {
                return Err(MappingError::RequestMismatch {
                    expected: event_request,
                    observed: request,
                });
            }
            if !dto_basis.same_authority(basis) {
                return Err(MappingError::BasisMismatch {
                    expected: basis,
                    observed: dto_basis,
                });
            }
            // A product surface reply is page data for one identity (a
            // package, its versions, its dependents). It is never the Orbit
            // catalog: that slot is owned by the root/index `Explore` read.
            // Page data lands in the keyed `DataStore`, not in the snapshot.
            Ok(current.with_key(current.key(), None))
        }
        EngineDto::Index {
            request,
            basis: dto_basis,
            key,
            revision,
            delta,
            project,
            project_state,
            catalog,
            files_indexed,
        } => {
            if request != event_request {
                return Err(MappingError::RequestMismatch {
                    expected: event_request,
                    observed: request,
                });
            }
            if !dto_basis.same_authority(basis) {
                return Err(MappingError::BasisMismatch {
                    expected: basis,
                    observed: dto_basis,
                });
            }
            if key.revision() != revision {
                return Err(MappingError::Engine(EngineFault::Failed(
                    crate::core::ErrorValue::new(
                        crate::core::FaultCode::Protocol,
                        "index DTO generation did not match its service cursor",
                    ),
                )));
            }
            if current.workspace().projects.iter().any(|row| row.id == project && row.request == Some(request) && row.operation.is_some()) {
                return Ok(mark_index_unconfirmed(current, request, &project,
                    "The index reply did not carry this operation's durable receipt. Check the saved operation before starting another."));
            }
            // A project result may arrive after a newer project has published.
            // Keep its row terminal evidence, while admitting the root only
            // when its producer authority is current.
            let admit_projection = !key.is_older_authority(current.key());
            let snapshot = if admit_projection {
                let snapshot = current.with_key(key, delta);
                let snapshot = project_state.map_or(snapshot.clone(), |project_state| {
                    snapshot.with_project(project_state.into_model(), key)
                });
                catalog.map_or(snapshot.clone(), |packages| {
                    snapshot.with_catalog(CatalogState { packages }, key)
                })
            } else {
                current.clone()
            };
            // A re-index is the user's signal that the folder changed; its
            // manifest facts are read again on the next dossier render.
            let snapshot = if snapshot
                .local_package()
                .loaded_value()
                .is_some_and(|package| package.project == project)
            {
                snapshot.with_local_package(Resource::not_yet())
            } else {
                snapshot
            };
            Ok(mark_index_ready(
                &snapshot,
                request,
                &project,
                files_indexed,
            ))
        }
        EngineDto::IndexOperation { request, basis: dto_basis, project, operation, observation } => {
            if request != event_request { return Err(MappingError::RequestMismatch { expected: event_request, observed: request }); }
            if !dto_basis.same_authority(basis) { return Err(MappingError::BasisMismatch { expected: basis, observed: dto_basis }); }
            if !operation.belongs_to(&project) || !operation.admits_observation(&observation) {
                return Ok(mark_index_unconfirmed(current, event_request, &project,
                    "The owner reply did not match this saved index operation. Its exact outcome is still unconfirmed."));
            }
            // Work identity survives root advances, while request/key/payload
            // membership rejects a late answer for a removed or replaced row.
            let mut workspace = current.workspace().clone();
            let mut rows = workspace.projects.to_vec();
            let Some(row) = rows.iter_mut().find(|row| row.id == project && row.request == Some(request)
                && row.operation.as_ref().is_some_and(|saved| saved.same_request(&operation))) else { return Ok(current.clone()); };
            let Some(observation) = row.operation.as_ref().and_then(|saved| saved.observation_for(observation)) else { return Ok(current.clone()); };
            use backend_library::{IndexOperationFailureReason, IndexOperationObservation, IndexOperationState};
            let (phase, error) = match &observation {
                IndexOperationObservation::Known(status) => match &status.state {
                    IndexOperationState::Accepted => (ProjectPhase::Indexing, None),
                    IndexOperationState::Active { .. } => (ProjectPhase::Indexing, None),
                    IndexOperationState::Published(_) => (ProjectPhase::Ready, None),
                    IndexOperationState::Failed { reason, detail } => (
                        if *reason == IndexOperationFailureReason::Cancelled { ProjectPhase::Cancelled } else { ProjectPhase::Failed },
                        Some(Arc::from(detail.as_str())),
                    ),
                    IndexOperationState::Unresolved { detail, .. } => (ProjectPhase::Unconfirmed, Some(Arc::from(detail.as_str()))),
                },
                IndexOperationObservation::OutsideReceiptWindow { .. } => (ProjectPhase::Unconfirmed,
                    Some(Arc::from("This operation key was consumed, but its terminal receipt is outside the owner's retained evidence window. Its publication outcome is unknown. A new index has not been started."))),
                IndexOperationObservation::Unknown { .. } => (ProjectPhase::Unconfirmed,
                    Some(Arc::from("The owner has no retained receipt for this saved operation. Its outcome is unknown; another index has not been started."))),
            };
            row.phase = phase;
            row.progress = None; // a stage is not a percentage
            row.files_indexed = None; // global health is not a per-operation count
            row.request = None;
            row.error = error;
            row.recent = true;
            if let Some(saved) = row.operation.as_mut() { saved.observation = Some(observation); }
            if phase == ProjectPhase::Ready && workspace.active.as_ref() == Some(&project) { workspace.host = Some(project.clone()); }
            workspace.projects = rows.into();
            let snapshot = current.with_workspace(workspace);
            // A new published attempt may have changed local manifest facts.
            // Re-read those local facts independently of current view hydration.
            let snapshot = if phase == ProjectPhase::Ready && snapshot.local_package().loaded_value()
                .is_some_and(|package| package.project == project) {
                snapshot.with_local_package(Resource::not_yet())
            } else { snapshot };
            Ok(snapshot)
        }
        EngineDto::LocalPackage {
            request,
            basis: dto_basis,
            package,
        } => {
            if request != event_request {
                return Err(MappingError::RequestMismatch {
                    expected: event_request,
                    observed: request,
                });
            }
            if !dto_basis.same_authority(basis) {
                return Err(MappingError::BasisMismatch {
                    expected: basis,
                    observed: dto_basis,
                });
            }
            Ok(current.with_local_package(Resource::loaded_arc_at(package, current.key())))
        }
    }
}

fn mark_index_ready(
    snapshot: &AppSnapshot,
    request: RequestId,
    project: &crate::core::LocalProjectId,
    files: Option<u64>,
) -> AppSnapshot {
    let mut workspace = snapshot.workspace().clone();
    if workspace.active.as_ref() == Some(project) {
        workspace.host = Some(project.clone());
    }
    workspace.path_error = None;
    workspace.projects = workspace
        .projects
        .iter()
        .cloned()
        .map(|mut item| {
            if item.id == *project && item.request == Some(request) {
                item.phase = ProjectPhase::Ready;
                item.progress = None;
                item.files_indexed = files;
                item.request = None;
                item.error = None;
                item.recent = true;
            }
            item
        })
        .collect::<Vec<_>>()
        .into();
    snapshot.with_workspace(workspace)
}

fn mark_index_cancelled(
    snapshot: &AppSnapshot,
    request: RequestId,
    project: &crate::core::LocalProjectId,
) -> AppSnapshot {
    let mut workspace = snapshot.workspace().clone();
    workspace.projects = workspace
        .projects
        .iter()
        .cloned()
        .map(|mut item| {
            if item.id == *project && item.request == Some(request) {
                // A legacy transport result cannot settle a caller-keyed
                // mutation. Only its checked exact operation receipt can.
                item.phase = if item.operation.is_some() {
                    ProjectPhase::Unconfirmed
                } else {
                    ProjectPhase::Cancelled
                };
                item.progress = None;
                item.request = None;
                item.error = item.operation.as_ref().map(|_| Arc::from(
                    "Cancellation returned without an exact operation receipt. Check the saved index operation before starting another index.",
                ));
            }
            item
        })
        .collect::<Vec<_>>()
        .into();
    snapshot.with_workspace(workspace)
}

fn mark_index_failed(
    snapshot: &AppSnapshot,
    request: RequestId,
    project: &crate::core::LocalProjectId,
    message: &str,
) -> AppSnapshot {
    let mut workspace = snapshot.workspace().clone();
    workspace.projects = workspace
        .projects
        .iter()
        .cloned()
        .map(|mut item| {
            if item.id == *project && item.request == Some(request) {
                item.phase = if item.operation.is_some() {
                    ProjectPhase::Unconfirmed
                } else {
                    ProjectPhase::Failed
                };
                item.progress = None;
                item.files_indexed = None;
                item.request = None;
                item.error = Some(Arc::from(if item.operation.is_some() {
                    "The index request failed without an exact operation receipt. Check the saved index operation before starting another index."
                } else {
                    message
                }));
            }
            item
        })
        .collect::<Vec<_>>()
        .into();
    snapshot.with_workspace(workspace)
}

fn mark_index_unconfirmed(
    snapshot: &AppSnapshot,
    request: RequestId,
    project: &crate::core::LocalProjectId,
    message: &str,
) -> AppSnapshot {
    let mut workspace = snapshot.workspace().clone();
    workspace.projects = workspace.projects.iter().cloned().map(|mut item| {
        if item.id == *project && item.request == Some(request) {
            item.phase = ProjectPhase::Unconfirmed;
            item.progress = None;
            item.request = None;
            item.error = Some(Arc::from(message));
        }
        item
    }).collect::<Vec<_>>().into();
    snapshot.with_workspace(workspace)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::navigation::RequestId;

    fn root() -> VersionedRoot {
        VersionedRoot::synthetic(
            backend_library::view_state_root(&[("mapping".to_owned(), "one".to_owned())]),
            4,
        )
    }

    /// A publication the owner made after the window's root: a new view
    /// root (a hash) at the next sequence, as the real owner answers a root
    /// read once a package it indexed is published.
    fn published_after(current: VersionedRoot, salt: &str) -> VersionedRoot {
        VersionedRoot::from_revision(
            current.producer_epoch(),
            backend_library::Cursor::at(
                backend_library::view_state_root(&[("mapping".to_owned(), salt.to_owned())]),
                current.generation() + 1,
            ),
            current.observation(),
        )
    }

    fn saved_operation() -> (AppSnapshot, crate::core::LocalProjectId, crate::model::IndexOperationClaim, RequestId) {
        let project = crate::core::LocalProjectId::new("/fixture/keyed-mapping").expect("project");
        let operation = crate::model::index_operation::tests::claim(&project, 0x61);
        let request = RequestId::new(70);
        let added = crate::navigation::reduce(&AppSnapshot::empty(root()),
            crate::navigation::Intent::AddProject { project: project.clone() }).snapshot;
        let submitted = crate::navigation::reduce(&added, crate::navigation::Intent::IndexProject {
            project: project.clone(), operation: operation.clone(), basis: root(), request }).snapshot;
        (submitted, project, operation, request)
    }

    fn operation_event(project: crate::core::LocalProjectId, operation: crate::model::IndexOperationClaim,
        request: RequestId, observation: backend_library::IndexOperationObservation) -> EngineEvent {
        EngineEvent { basis: root(), request, lane: None, result: Ok(EngineDto::IndexOperation {
            request, basis: root(), project, operation, observation }) }
    }

    #[test]
    fn legacy_terminal_faults_keep_exact_claim_unconfirmed_and_refuse_another_mutation() {
        use crate::navigation::{Effect, Intent};

        let (submitted, project, operation, request) = saved_operation();
        let faults = [
            EngineFault::IndexFailed {
                project: project.clone(),
                error: crate::core::ErrorValue::new(crate::core::FaultCode::Transport, "reply lost"),
            },
            EngineFault::IndexCancelled {
                project: project.clone(),
            },
        ];
        for fault in faults {
            let event = EngineEvent {
                basis: root(),
                request,
                lane: None,
                result: Err(fault.clone()),
            };
            let received = map_event(&submitted, event).expect("represented failure");
            let row = &received.workspace().projects[0];
            assert_eq!(row.phase, ProjectPhase::Unconfirmed);
            assert_eq!(row.operation, Some(operation.clone()));
            assert_eq!(row.request, None);
            assert!(row.error.is_some());

            let retried = crate::navigation::reduce(&received, Intent::RetryIndex(project.clone()));
            assert_eq!(retried.snapshot.workspace().projects[0].phase, ProjectPhase::Unconfirmed);
            assert_eq!(retried.snapshot.workspace().projects[0].operation, Some(operation.clone()));
            assert!(retried.snapshot.workspace().path_error.is_some());
            assert!(!retried.effects.iter().any(|effect| matches!(effect, Effect::Engine(_))));

            let replacement = crate::model::index_operation::tests::claim(&project, 0x62);
            let direct = crate::navigation::reduce(&retried.snapshot, Intent::IndexProject {
                project: project.clone(),
                operation: replacement,
                basis: root(),
                request: RequestId::new(71),
            });
            assert_eq!(direct.snapshot.workspace().projects[0].operation, Some(operation.clone()));
            assert!(!direct.effects.iter().any(|effect| matches!(effect, Effect::Engine(_))));

            let stale = map_event(&submitted, EngineEvent {
                basis: root(),
                request: RequestId::new(69),
                lane: None,
                result: Err(fault),
            }).expect("stale fault ignored");
            assert_eq!(stale, submitted);
        }
    }

    #[test]
    fn checked_terminal_receipt_still_allows_an_explicit_new_attempt() {
        use crate::navigation::{Effect, EngineCommand, Intent};

        let (submitted, project, operation, request) = saved_operation();
        let observation = crate::model::index_operation::tests::observation(
            &operation,
            backend_library::IndexOperationState::Failed {
                reason: backend_library::IndexOperationFailureReason::Cancelled,
                detail: backend_library::ProductText::from_static("owner confirmed cancellation before publication"),
            },
        );
        let received = map_event(&submitted, operation_event(
            project.clone(), operation, request, observation.clone(),
        )).expect("checked exact cancellation receipt");
        assert_eq!(received.workspace().projects[0].phase, ProjectPhase::Cancelled);
        assert_eq!(received.workspace().projects[0].operation.as_ref()
            .and_then(|claim| claim.observation.as_ref()), Some(&observation));

        let prepared = crate::navigation::reduce(&received, Intent::RetryIndex(project.clone()));
        let replacement = crate::model::index_operation::tests::claim(&project, 0x62);
        let started = crate::navigation::reduce(&prepared.snapshot, Intent::IndexProject {
            project,
            operation: replacement.clone(),
            basis: root(),
            request: RequestId::new(71),
        });
        assert_eq!(started.snapshot.workspace().projects[0].operation, Some(replacement));
        assert!(started.effects.iter().any(|effect| matches!(effect,
            Effect::Engine(EngineCommand::IndexProject { .. }))));
    }

    #[test]
    fn unkeyed_compatibility_faults_preserve_their_terminal_semantics() {
        let (submitted, project, _, request) = saved_operation();
        let mut workspace = submitted.workspace().clone();
        let mut rows = workspace.projects.to_vec();
        rows[0].operation = None;
        workspace.projects = rows.into();
        let unkeyed = submitted.with_workspace(workspace);
        for (fault, phase, message) in [
            (EngineFault::IndexFailed {
                project: project.clone(),
                error: crate::core::ErrorValue::new(crate::core::FaultCode::Transport, "reply lost"),
            }, ProjectPhase::Failed, Some("reply lost")),
            (EngineFault::IndexCancelled { project: project.clone() }, ProjectPhase::Cancelled, None),
        ] {
            let received = map_event(&unkeyed, EngineEvent {
                basis: root(), request, lane: None, result: Err(fault),
            }).expect("compatibility fault");
            let row = &received.workspace().projects[0];
            assert_eq!(row.phase, phase);
            assert_eq!(row.operation, None);
            assert_eq!(row.request, None);
            assert_eq!(row.error.as_deref(), message);
        }
    }

    #[test]
    fn exact_publication_finishes_saved_work_after_root_advance_without_adopting_its_root() {
        let (submitted, project, operation, request) = saved_operation();
        let current_key = published_after(root(), "unrelated-current-view");
        let current = submitted.with_key(current_key, None);
        let observation = crate::model::index_operation::tests::published(&operation);
        let received = map_event(&current, operation_event(project, operation.clone(), request, observation.clone())).expect("exact receipt");
        let row = &received.workspace().projects[0];
        assert_eq!(row.phase, ProjectPhase::Ready);
        assert_eq!(row.request, None);
        assert_eq!(row.files_indexed, None);
        assert_eq!(row.operation.as_ref().and_then(|claim| claim.observation.as_ref()), Some(&observation));
        assert_eq!(received.key(), current_key, "receipt settles work, not current content authority");
    }

    #[test]
    fn archived_exact_key_is_unconfirmed_and_cannot_erase_an_owned_publication() {
        let (current, project, operation, request) = saved_operation();
        let outside = crate::model::index_operation::tests::outside(&operation);
        let received = map_event(&current, operation_event(project.clone(), operation.clone(), request, outside.clone())).expect("archived");
        let row = &received.workspace().projects[0];
        assert_eq!(row.phase, ProjectPhase::Unconfirmed);
        assert_eq!(row.request, None);
        assert_eq!(row.index_status_text(), Some("Index receipt is outside the evidence window"));
        assert!(!row.operation.as_ref().expect("claim").needs_observation());
        assert_eq!(received.key(), current.key(), "an archived key never adopts a content root");
        let mut workspace = current.workspace().clone();
        let mut rows = workspace.projects.to_vec();
        let receipt = crate::model::index_operation::tests::published(&operation);
        rows[0].operation.as_mut().expect("saved claim").observation = Some(receipt.clone());
        workspace.projects = rows.into();
        let held = current.with_workspace(workspace);
        let received = map_event(&held, operation_event(project, operation, request, outside)).expect("held receipt");
        assert_eq!(received.workspace().projects[0].phase, ProjectPhase::Ready);
        assert_eq!(received.workspace().projects[0].operation.as_ref().and_then(|claim| claim.observation.as_ref()), Some(&receipt));
        assert_eq!(received.key(), held.key());
    }

    #[test]
    fn accepted_unknown_wrong_payload_and_replaced_key_never_invent_publication() {
        let (current, project, operation, request) = saved_operation();
        let accepted = crate::model::index_operation::tests::observation(&operation, backend_library::IndexOperationState::Accepted);
        let received = map_event(&current, operation_event(project.clone(), operation.clone(), request, accepted)).expect("accepted");
        assert_eq!(received.workspace().projects[0].phase, ProjectPhase::Indexing);
        assert!(received.workspace().projects[0].operation.as_ref().expect("claim").needs_observation());
        let unknown = backend_library::IndexOperationObservation::Unknown { operation_key: operation.key };
        let received = map_event(&current, operation_event(project.clone(), operation.clone(), request, unknown)).expect("unknown");
        assert_eq!(received.workspace().projects[0].phase, ProjectPhase::Unconfirmed);
        assert!(!received.workspace().projects[0].operation.as_ref().expect("claim").needs_observation());
        let other = crate::core::LocalProjectId::new("/fixture/wrong-payload").expect("project");
        let wrong_payload = crate::model::IndexOperationClaim::for_project(operation.key, &other).expect("other payload");
        let wrong_receipt = crate::model::index_operation::tests::published(&wrong_payload);
        let received = map_event(&current, operation_event(project.clone(), operation.clone(), request, wrong_receipt)).expect("held");
        assert_eq!(received.workspace().projects[0].phase, ProjectPhase::Unconfirmed);
        assert_eq!(received.workspace().projects[0].operation, Some(operation.clone()));
        let replacement = crate::model::index_operation::tests::claim(&project, 0x62);
        let late_receipt = crate::model::index_operation::tests::published(&replacement);
        let received = map_event(&current, operation_event(project, replacement, request, late_receipt)).expect("stale ignored");
        assert_eq!(received, current, "another key cannot finish this row even with the same transport request");
    }

    #[test]
    fn a_later_publication_is_admitted_whichever_way_its_root_hash_sorts() {
        // Every root read after a package was indexed carried a new root hash
        // and the next sequence. Ordering by the whole cursor compared the
        // hashes first, so about half of them were rejected as older and the
        // Library never grew while packages were added.
        let current_key = root().with_generation(2);
        let current = AppSnapshot::empty(current_key);
        let mut seen = [false; 2];
        for salt in 0..32 {
            let key = published_after(current_key, &format!("publication {salt}"));
            seen[usize::from(key.revision() < current_key.revision())] = true;
            let accepted = map_event(
                &current,
                EngineEvent {
                    basis: current_key,
                    request: RequestId::new(1),
                    lane: None,
                    result: Ok(EngineDto::Root {
                        request: RequestId::new(1),
                        basis: current_key,
                        key,
                        revision: key.revision(),
                        delta: None,
                        project: None,
                        catalog: None,
                    }),
                },
            );
            assert!(accepted.is_ok_and(|snapshot| snapshot.key() == key), "publication {salt}, the next sequence, is admitted");
        }
        assert_eq!(seen, [true, true], "the roots sort both ways against the current one");
    }

    #[test]
    fn observation_sequence_does_not_authorize_or_reject_a_result() {
        let current_key = root().with_generation(2);
        let current = AppSnapshot::empty(current_key);
        let basis = current_key.observed_at(99);
        let accepted = map_event(
            &current,
            EngineEvent {
                basis,
                request: RequestId::new(1),
                lane: None,
                result: Ok(EngineDto::Root {
                    request: RequestId::new(1),
                    basis,
                    key: basis.with_generation(3).observed_at(100),
                    revision: basis.with_generation(3).revision(),
                    delta: None,
                    project: None,
                    catalog: None,
                }),
            },
        )
        .expect("same producer authority");
        assert_eq!(accepted.key().generation(), 3);
        assert_eq!(accepted.key().observation(), 100);
    }

    #[test]
    fn older_producer_generation_is_rejected_even_if_observed_later() {
        let current_key = root().with_generation(2);
        let current = AppSnapshot::empty(current_key);
        let basis = current_key.observed_at(99);
        let error = map_event(
            &current,
            EngineEvent {
                basis,
                request: RequestId::new(1),
                lane: None,
                result: Ok(EngineDto::Root {
                    request: RequestId::new(1),
                    basis,
                    key: basis.with_generation(1).observed_at(1000),
                    revision: basis.with_generation(1).revision(),
                    delta: None,
                    project: None,
                    catalog: None,
                }),
            },
        )
        .expect_err("older generation");
        assert!(matches!(error, MappingError::StaleRoot { .. }));
    }

    #[test]
    fn mismatched_dto_request_is_rejected_before_mapping() {
        let current_key = root().with_generation(2);
        let current = AppSnapshot::empty(current_key);
        let request = RequestId::new(1);
        let observed = RequestId::new(2);
        let error = map_event(
            &current,
            EngineEvent {
                basis: current_key,
                request,
                lane: None,
                result: Ok(EngineDto::Root {
                    request: observed,
                    basis: current_key,
                    key: current_key.with_generation(3),
                    revision: current_key.with_generation(3).revision(),
                    delta: None,
                    project: None,
                    catalog: None,
                }),
            },
        )
        .expect_err("request envelope mismatch");
        assert_eq!(
            error,
            MappingError::RequestMismatch {
                expected: request,
                observed,
            }
        );
    }
}
