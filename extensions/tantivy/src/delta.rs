//! Exact lexical relation state and delta admission.

use crate::{Binding, Error, IndexRelation, Limits};
use backend_version::{CoverageWitness, Delta as VersionDelta, MapChange, RelationState};
use std::collections::BTreeSet;

/// One deterministic document mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DocumentChange {
    /// Inserts or replaces a complete document.
    Add {
        /// Stable logical document key.
        id: u64,
        /// Field/value pairs.
        fields: Vec<(String, String)>,
    },
    /// Removes an existing document.
    Delete {
        /// Stable logical document key.
        id: u64,
    },
}

impl DocumentChange {
    pub(crate) fn id(&self) -> u64 {
        match self {
            Self::Add { id, .. } | Self::Delete { id } => *id,
        }
    }
}

/// A checked document relation transition bound to exact inputs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentDelta {
    /// Exact lexical materialization inputs.
    pub binding: Binding,
    /// Typed before/after relation delta.
    pub delta: VersionDelta<IndexRelation>,
    /// Coverage of the changed scope.
    pub coverage: CoverageWitness,
}

/// A complete lexical document snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentState {
    binding: Binding,
    state: RelationState<IndexRelation>,
    coverage: CoverageWitness,
    limits: Limits,
}

impl DocumentState {
    /// Builds a complete document relation state.
    ///
    /// # Errors
    ///
    /// Returns a typed error when coverage, IDs, fields, duplicate keys,
    /// payload sizes, or the claimed root is invalid.
    pub fn new(
        binding: Binding,
        coverage: CoverageWitness,
        documents: Vec<(u64, Vec<(String, String)>)>,
        limits: Limits,
    ) -> Result<Self, Error> {
        let limits = limits.validate()?;
        if !matches!(
            coverage,
            CoverageWitness::Complete(_) | CoverageWitness::Closed(_)
        ) {
            return Err(Error::IncompleteCoverage);
        }
        if documents.len() > limits.max_documents {
            return Err(Error::SizeLimit);
        }
        let mut seen = BTreeSet::new();
        let mut entries = Vec::with_capacity(documents.len());
        let mut total = 0usize;
        for (id, fields) in documents {
            if id == 0 || !seen.insert(id) {
                return Err(Error::MalformedInput);
            }
            let fields = normalize_fields(fields, limits, &mut total)?;
            entries.push((id, fields));
        }
        entries.sort_by_key(|(id, _)| *id);
        let state = RelationState::from_entries(entries, coverage).map_err(Error::State)?;
        validate_document_state(&state, limits)?;
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

    /// Returns the exact binding of this state.
    #[must_use]
    pub const fn binding(&self) -> Binding {
        self.binding
    }

    /// Returns the complete coverage witness.
    #[must_use]
    pub const fn coverage(&self) -> CoverageWitness {
        self.coverage
    }

    /// Returns visible documents in canonical key order.
    pub fn iter(&self) -> impl Iterator<Item = (u64, &[(String, String)])> {
        self.state
            .iter()
            .map(|(id, fields)| (*id, fields.as_slice()))
    }

    /// Prepares a checked exact delta from this state.
    ///
    /// # Errors
    ///
    /// Returns a typed error for malformed/duplicate changes, missing deletes,
    /// size violations, or stale before-values.
    pub fn prepare_delta(&self, changes: Vec<DocumentChange>) -> Result<DocumentDelta, Error> {
        let mut changes = changes;
        if changes.len() > self.limits.max_documents {
            return Err(Error::SizeLimit);
        }
        if changes.iter().any(|change| change.id() == 0) {
            return Err(Error::MalformedInput);
        }
        changes.sort_by_key(DocumentChange::id);
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
                let key = change.id();
                let before = self.state.get(&key).cloned();
                match change {
                    DocumentChange::Add { fields, .. } => {
                        let fields = normalize_fields(fields, self.limits, &mut total)?;
                        Ok(MapChange {
                            key,
                            before,
                            after: Some(fields),
                        })
                    }
                    DocumentChange::Delete { .. } => {
                        if before.is_none() {
                            return Err(Error::MissingDocument);
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
        validate_document_state(&next, self.limits)?;
        let mut binding = self.binding;
        binding.root = delta.target();
        Ok(DocumentDelta {
            binding,
            delta,
            coverage: self.coverage,
        })
    }

    /// Applies an exact document delta after checking every binding field.
    ///
    /// # Errors
    ///
    /// Returns [`Error::StaleRoot`] for a stale binding/base root, or a typed
    /// transition error when lower admission rejects the delta.
    pub fn apply_delta(&self, change: &DocumentDelta) -> Result<Self, Error> {
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
        validate_document_state(&state, self.limits)?;
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

fn validate_document_state(
    state: &RelationState<IndexRelation>,
    limits: Limits,
) -> Result<(), Error> {
    let mut count = 0usize;
    let mut total = 0usize;
    for (id, fields) in state.iter() {
        if *id == 0 {
            return Err(Error::MalformedInput);
        }
        let _ = normalize_fields(fields.clone(), limits, &mut total)?;
        count = count.checked_add(1).ok_or(Error::SizeLimit)?;
    }
    if count > limits.max_documents {
        return Err(Error::SizeLimit);
    }
    Ok(())
}

pub(crate) fn normalize_fields(
    mut fields: Vec<(String, String)>,
    limits: Limits,
    total: &mut usize,
) -> Result<Vec<(String, String)>, Error> {
    if fields.len() > limits.max_fields_per_document {
        return Err(Error::SizeLimit);
    }
    if fields.iter().any(|(field, _)| field.is_empty()) {
        return Err(Error::MalformedInput);
    }
    if fields.iter().any(|(field, text)| {
        field.len() > limits.max_field_bytes || text.len() > limits.max_field_bytes
    }) {
        return Err(Error::SizeLimit);
    }
    fields.sort();
    if fields.windows(2).any(|window| window[0].0 == window[1].0) {
        return Err(Error::MalformedInput);
    }
    for (field, text) in &fields {
        *total = total
            .checked_add(field.len())
            .and_then(|value| value.checked_add(text.len()))
            .ok_or(Error::SizeLimit)?;
    }
    if *total > limits.max_total_text_bytes {
        return Err(Error::SizeLimit);
    }
    Ok(fields)
}
