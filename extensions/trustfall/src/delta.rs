//! Exact graph relation state and delta admission.

use crate::contracts::{GraphChange, GraphRow};
use crate::identity::SemanticRelation;
use crate::{Binding, Error, Limits};
use backend_version::{CoverageWitness, Delta as VersionDelta, MapChange, RelationState};
use std::collections::BTreeSet;

/// Checked graph relation delta bound to exact inputs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphDelta {
    /// Exact graph binding with the target root.
    pub binding: Binding,
    /// Typed before/after relation transition.
    pub delta: VersionDelta<SemanticRelation>,
    /// Coverage of the changed graph scope.
    pub coverage: CoverageWitness,
}

/// Complete graph relation state for local deterministic execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphState {
    binding: Binding,
    state: RelationState<SemanticRelation>,
    coverage: CoverageWitness,
    limits: Limits,
}

impl GraphState {
    /// Builds a complete graph state from canonical rows.
    ///
    /// # Errors
    ///
    /// Returns a typed error when row keys, values, coverage, sizes, or the
    /// claimed exact root are invalid.
    pub fn new(
        binding: Binding,
        coverage: CoverageWitness,
        rows: Vec<GraphRow>,
        limits: Limits,
    ) -> Result<Self, Error> {
        let limits = limits.validate()?;
        if !matches!(
            coverage,
            CoverageWitness::Complete(_) | CoverageWitness::Closed(_)
        ) {
            return Err(Error::IncompleteCoverage);
        }
        if rows.len() > limits.max_rows {
            return Err(Error::SizeLimit);
        }
        let mut seen = BTreeSet::new();
        let mut total = 0usize;
        let mut entries = Vec::with_capacity(rows.len());
        for row in &rows {
            normalize_row(row, limits, &mut total)?;
            if row.key == 0 || !seen.insert(row.key) {
                return Err(Error::MalformedInput);
            }
            entries.push((row.key, row.values.clone()));
        }
        entries.sort_by_key(|(key, _)| *key);
        let state = RelationState::from_entries(entries, coverage).map_err(Error::State)?;
        validate_graph_state(&state, limits)?;
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

    /// Returns this state's exact binding.
    #[must_use]
    pub const fn binding(&self) -> Binding {
        self.binding
    }

    /// Returns the complete graph coverage witness.
    #[must_use]
    pub const fn coverage(&self) -> CoverageWitness {
        self.coverage
    }

    /// Iterates rows in stable logical key order.
    pub fn iter(&self) -> impl Iterator<Item = GraphRow> + '_ {
        self.state.iter().map(|(key, values)| GraphRow {
            key: *key,
            values: values.clone(),
        })
    }

    /// Prepares a checked exact graph delta.
    ///
    /// # Errors
    ///
    /// Returns a typed error for malformed/duplicate changes, missing deletes,
    /// row size violations, or stale before-values.
    pub fn prepare_delta(&self, changes: Vec<GraphChange>) -> Result<GraphDelta, Error> {
        let mut changes = changes;
        if changes.len() > self.limits.max_rows {
            return Err(Error::SizeLimit);
        }
        if changes.iter().any(|change| change.key() == 0) {
            return Err(Error::MalformedInput);
        }
        changes.sort_by_key(GraphChange::key);
        if changes
            .windows(2)
            .any(|window| window[0].key() == window[1].key())
        {
            return Err(Error::MalformedInput);
        }
        let mut total = 0usize;
        let map_changes = changes
            .into_iter()
            .map(|change| {
                let key = change.key();
                let before = self.state.get(&key).cloned();
                match change {
                    GraphChange::Upsert { values, .. } => {
                        let row = GraphRow { key, values };
                        normalize_row(&row, self.limits, &mut total)?;
                        Ok(MapChange {
                            key,
                            before,
                            after: Some(row.values),
                        })
                    }
                    GraphChange::Delete { .. } => {
                        if before.is_none() {
                            return Err(Error::MissingRow);
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
        validate_graph_state(&next, self.limits)?;
        let mut binding = self.binding;
        binding.root = delta.target();
        Ok(GraphDelta {
            binding,
            delta,
            coverage: self.coverage,
        })
    }

    /// Applies a graph delta only at its exact source root and input binding.
    ///
    /// # Errors
    ///
    /// Returns [`Error::StaleRoot`] when any input identity or base root is
    /// stale, or a typed lower transition error.
    pub fn apply_delta(&self, change: &GraphDelta) -> Result<Self, Error> {
        if change.binding.workspace != self.binding.workspace
            || change.binding.recipe != self.binding.recipe
            || change.binding.authority != self.binding.authority
            || change.binding.read_manifest != self.binding.read_manifest
            || change.binding.frontier != self.binding.frontier
            || change.delta.base() != self.binding.root
            || !change.delta.is_canonical()
        {
            return Err(Error::StaleRoot);
        }
        if change.coverage != self.coverage
            || !matches!(
                change.coverage,
                CoverageWitness::Complete(_) | CoverageWitness::Closed(_)
            )
        {
            return Err(Error::IncompleteCoverage);
        }
        if change.binding.root != change.delta.target() {
            return Err(Error::StaleRoot);
        }
        let state =
            backend_version::apply_delta(&self.state, &change.delta).map_err(Error::Delta)?;
        validate_graph_state(&state, self.limits)?;
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

fn validate_graph_state(
    state: &RelationState<SemanticRelation>,
    limits: Limits,
) -> Result<(), Error> {
    let mut count = 0usize;
    let mut total = 0usize;
    for (key, values) in state.iter() {
        // Validation runs after every incremental transition.  Do not clone
        // each retained value vector merely to pass it through the public row
        // normalizer: on a large graph that turns a one-row delta into an
        // O(total retained bytes) allocation burst.  The relation state has
        // already admitted the key/value ownership; this pass only needs to
        // inspect the borrowed values against the same limits.
        validate_values(*key, values, limits, &mut total)?;
        count = count.checked_add(1).ok_or(Error::SizeLimit)?;
    }
    if count > limits.max_rows {
        return Err(Error::SizeLimit);
    }
    Ok(())
}

pub(crate) fn normalize_row(
    row: &GraphRow,
    limits: Limits,
    total: &mut usize,
) -> Result<(), Error> {
    validate_values(row.key, &row.values, limits, total)
}

fn validate_values(
    key: u64,
    values: &[String],
    limits: Limits,
    total: &mut usize,
) -> Result<(), Error> {
    if key == 0 {
        return Err(Error::MalformedInput);
    }
    if values.len() > limits.max_fields_per_row {
        return Err(Error::SizeLimit);
    }
    if values
        .iter()
        .any(|value| value.len() > limits.max_field_bytes)
    {
        return Err(Error::SizeLimit);
    }
    *total = values.iter().try_fold(*total, |sum, value| {
        sum.checked_add(value.len()).ok_or(Error::SizeLimit)
    })?;
    if *total > limits.max_total_bytes {
        return Err(Error::SizeLimit);
    }
    Ok(())
}
