//! Exact candidate relation state and delta admission.

use crate::identity::CandidateRelation;
use crate::{Binding, CandidateId, Error, Limits};
use backend_version::{CoverageWitness, Delta as VersionDelta, MapChange, RelationState};
use std::collections::BTreeSet;

/// A typed candidate relation transition with its exact query binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CandidateDelta {
    /// Exact query/materialization inputs.
    pub binding: Binding,
    /// Checked before/after relation transition.
    pub delta: VersionDelta<CandidateRelation>,
    /// Coverage of the changed scope.
    pub coverage: CoverageWitness,
}

/// A complete candidate relation snapshot owned by a deterministic adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CandidateState {
    binding: Binding,
    state: RelationState<CandidateRelation>,
    coverage: CoverageWitness,
    limits: Limits,
}

impl CandidateState {
    /// Builds a complete state from logical candidate payloads.
    ///
    /// # Errors
    ///
    /// Returns a typed error when coverage, candidate identities, payload
    /// sizes, duplicate keys, or the claimed root are invalid.
    pub fn new(
        binding: Binding,
        coverage: CoverageWitness,
        candidates: Vec<(CandidateId, Vec<u8>)>,
        limits: Limits,
    ) -> Result<Self, Error> {
        let limits = limits.validate()?;
        if !coverage.state().is_complete() {
            return Err(Error::IncompleteCoverage);
        }
        if candidates.len() > limits.max_candidates {
            return Err(Error::SizeLimit);
        }
        let mut seen = BTreeSet::new();
        let mut entries = Vec::with_capacity(candidates.len());
        let mut total = 0usize;
        for (id, payload) in candidates {
            if !id.is_valid() || !seen.insert(id) {
                return Err(Error::MalformedInput);
            }
            if payload.len() > limits.max_payload_bytes {
                return Err(Error::SizeLimit);
            }
            total = total.checked_add(payload.len()).ok_or(Error::SizeLimit)?;
            if total > limits.max_total_payload_bytes {
                return Err(Error::SizeLimit);
            }
            entries.push((id.0, payload));
        }
        entries.sort_by_key(|(id, _)| *id);
        let state = RelationState::from_entries(entries, coverage).map_err(Error::State)?;
        validate_candidate_state(&state, limits)?;
        if state.root() != binding.root {
            return Err(Error::StaleRoot);
        }
        Ok(Self {
            binding,
            state,
            coverage,
            limits,
        })
    }

    /// Returns the exact binding of this snapshot.
    #[must_use]
    pub const fn binding(&self) -> Binding {
        self.binding
    }

    /// Returns the complete coverage witness.
    #[must_use]
    pub const fn coverage(&self) -> CoverageWitness {
        self.coverage
    }

    /// Returns canonical visible candidate payloads.
    pub fn iter(&self) -> impl Iterator<Item = (CandidateId, &[u8])> {
        self.state
            .iter()
            .map(|(id, payload)| (CandidateId(*id), payload.as_slice()))
    }

    /// Prepares a checked delta against this exact state root.
    ///
    /// # Errors
    ///
    /// Returns a typed error for duplicate/malformed changes, missing deletes,
    /// size violations, or an invalid exact relation transition.
    pub fn prepare_delta(&self, changes: Vec<CandidateChange>) -> Result<CandidateDelta, Error> {
        let mut changes = changes;
        if changes.len() > self.limits.max_candidates {
            return Err(Error::SizeLimit);
        }
        if changes.iter().any(|change| !change.id().is_valid()) {
            return Err(Error::MalformedInput);
        }
        changes.sort_by_key(CandidateChange::id);
        if changes
            .windows(2)
            .any(|window| window[0].id() == window[1].id())
        {
            return Err(Error::MalformedInput);
        }
        let mut total = 0usize;
        let map_changes = changes
            .into_iter()
            .map(|change| {
                let key = change.id().0;
                let before = self.state.get(&key).cloned();
                match change {
                    CandidateChange::Upsert { payload, .. } => {
                        if payload.len() > self.limits.max_payload_bytes {
                            return Err(Error::SizeLimit);
                        }
                        total = total.checked_add(payload.len()).ok_or(Error::SizeLimit)?;
                        if total > self.limits.max_total_payload_bytes {
                            return Err(Error::SizeLimit);
                        }
                        Ok(MapChange {
                            key,
                            before,
                            after: Some(payload),
                        })
                    }
                    CandidateChange::Delete { .. } => {
                        if before.is_none() {
                            return Err(Error::MissingCandidate);
                        }
                        Ok(MapChange {
                            key,
                            before,
                            after: None,
                        })
                    }
                }
            })
            .collect::<Result<Vec<_>, Error>>()?;
        let delta =
            backend_version::prepare_delta(&self.state, map_changes).map_err(Error::Delta)?;
        let next = backend_version::apply_delta(&self.state, &delta).map_err(Error::Delta)?;
        validate_candidate_state(&next, self.limits)?;
        let mut binding = self.binding;
        binding.root = delta.target();
        Ok(CandidateDelta {
            binding,
            delta,
            coverage: self.coverage,
        })
    }

    /// Applies a delta only when its exact binding and base root match.
    ///
    /// # Errors
    ///
    /// Returns [`Error::StaleRoot`] for a different binding/base or a typed
    /// delta error when the transition fails validation.
    pub fn apply_delta(&self, change: &CandidateDelta) -> Result<Self, Error> {
        if change.binding.workspace != self.binding.workspace
            || change.binding.recipe != self.binding.recipe
            || change.binding.authority != self.binding.authority
            || change.binding.read_manifest != self.binding.read_manifest
            || change.binding.frontier != self.binding.frontier
            || change.delta.base() != self.binding.root
        {
            return Err(Error::StaleRoot);
        }
        if change.coverage != self.coverage || !change.coverage.state().is_complete() {
            return Err(Error::IncompleteCoverage);
        }
        if change.binding.root != change.delta.target() {
            return Err(Error::StaleRoot);
        }
        let state =
            backend_version::apply_delta(&self.state, &change.delta).map_err(Error::Delta)?;
        validate_candidate_state(&state, self.limits)?;
        let mut binding = self.binding;
        binding.root = state.root();
        Ok(Self {
            binding,
            state,
            coverage: self.coverage,
            limits: self.limits,
        })
    }
}

fn validate_candidate_state(
    state: &RelationState<CandidateRelation>,
    limits: Limits,
) -> Result<(), Error> {
    let mut count = 0usize;
    let mut total = 0usize;
    for (id, payload) in state.iter() {
        if *id == 0 {
            return Err(Error::MalformedInput);
        }
        if payload.len() > limits.max_payload_bytes {
            return Err(Error::SizeLimit);
        }
        count = count.checked_add(1).ok_or(Error::SizeLimit)?;
        total = total.checked_add(payload.len()).ok_or(Error::SizeLimit)?;
        if total > limits.max_total_payload_bytes {
            return Err(Error::SizeLimit);
        }
    }
    if count > limits.max_candidates {
        return Err(Error::SizeLimit);
    }
    Ok(())
}

/// One logical candidate insertion, replacement, or deletion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CandidateChange {
    /// Inserts or replaces one logical candidate payload.
    Upsert {
        /// Stable candidate identity.
        id: CandidateId,
        /// Complete immutable vector/metadata payload.
        payload: Vec<u8>,
    },
    /// Removes one existing logical candidate.
    Delete {
        /// Stable candidate identity.
        id: CandidateId,
    },
}

impl CandidateChange {
    fn id(&self) -> CandidateId {
        match self {
            Self::Upsert { id, .. } | Self::Delete { id } => *id,
        }
    }
}
