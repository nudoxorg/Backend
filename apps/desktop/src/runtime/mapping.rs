//! Engine DTO → immutable snapshot mapping.

use super::actor::{EngineDto, EngineEvent, EngineFault};
use super::client::package_summary;
use crate::core::VersionedRoot;
use crate::model::{AppSnapshot, CatalogState};
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
pub fn map_event(current: &AppSnapshot, event: EngineEvent) -> Result<AppSnapshot, MappingError> {
    let EngineEvent {
        basis,
        request: event_request,
        result,
        ..
    } = event;
    let dto = result.map_err(MappingError::Engine)?;
    match dto {
        EngineDto::Root {
            request,
            basis: dto_basis,
            key,
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
            reply,
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
            let packages = match reply {
                backend_library::SurfaceReply::Explored(records)
                | backend_library::SurfaceReply::Package(records)
                | backend_library::SurfaceReply::IndexSearch(records)
                | backend_library::SurfaceReply::PackageVersions(records) => Some(
                    records
                        .iter()
                        .filter_map(package_summary)
                        .collect::<Vec<_>>()
                        .into(),
                ),
                backend_library::SurfaceReply::PackageProfile { latest, .. } => latest
                    .as_ref()
                    .and_then(package_summary)
                    .map(|package| Arc::from([package])),
                _ => None,
            };
            let snapshot = packages.map_or_else(
                || current.with_key(current.key(), None),
                |packages| current.with_catalog(CatalogState { packages }, current.key()),
            );
            Ok(snapshot)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::navigation::RequestId;

    fn root() -> VersionedRoot {
        VersionedRoot::new(
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
                    delta: None,
                    project: None,
                    catalog: None,
                }),
            },
        )
        .expect("same producer authority");
        assert_eq!(accepted.key().generation, 3);
        assert_eq!(accepted.key().observation, 100);
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
