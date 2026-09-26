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
                item.phase = ProjectPhase::Cancelled;
                item.progress = None;
                item.request = None;
                item.error = None;
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
                item.phase = ProjectPhase::Failed;
                item.progress = None;
                item.files_indexed = None;
                item.request = None;
                item.error = Some(Arc::from(message));
            }
            item
        })
        .collect::<Vec<_>>()
        .into();
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
