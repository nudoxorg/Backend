//! Occurrence ledger construction, delta application, and accessors.

use super::*;

impl OccurrenceLedger {
    /// Builds a ledger from a complete scoped replacement.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::IncompleteCoverage`] without an admitted
    /// complete witness, [`SemanticError::DuplicateFact`] for repeated
    /// addresses, or [`SemanticError::InvalidSupport`] for zero weights.
    pub fn replace(
        coverage: FacetCoverage,
        occurrences: Vec<OccurrenceFact>,
    ) -> Result<Self, SemanticError> {
        if !coverage.authoritative() {
            return Err(SemanticError::IncompleteCoverage);
        }
        if coverage.deletion() != crate::Deletion::Live && !occurrences.is_empty() {
            return Err(SemanticError::InvalidCoverageState);
        }
        let mut rows = BTreeMap::new();
        for occurrence in occurrences {
            if occurrence.multiplicity == 0 || occurrence.support == 0 {
                return Err(SemanticError::InvalidSupport);
            }
            if rows.insert(occurrence.address(), (occurrence, 1)).is_some() {
                return Err(SemanticError::DuplicateFact);
            }
        }
        let rows = rows.into_iter().collect::<Vec<_>>();
        let edge_set = Self::derive_edges(coverage, &rows)?;
        let row_count = rows.len();
        let rows = RowState::from_entries(rows, coverage.witness()).map_err(map_row_state_error)?;
        Ok(Self {
            coverage,
            rows,
            row_count,
            edges: edge_set,
            work: OccurrenceWorkCounters::default(),
        })
    }

    fn derive_edges(
        coverage: FacetCoverage,
        rows: &[(OccurrenceAddress, (OccurrenceFact, i64))],
    ) -> Result<EdgeSet, SemanticError> {
        let mut supports: BTreeMap<EdgeKey, (u64, u64)> = BTreeMap::new();
        for (_, (occurrence, count)) in rows {
            let count = u64::try_from(*count).map_err(|_| SemanticError::Overflow)?;
            let key = EdgeKey {
                from: occurrence.source,
                to: occurrence.target,
                kind: EdgeKind::Reference,
            };
            let value = supports.entry(key).or_insert((0, 0));
            value.0 = value
                .0
                .checked_add(
                    u64::from(occurrence.multiplicity)
                        .checked_mul(count)
                        .ok_or(SemanticError::Overflow)?,
                )
                .ok_or(SemanticError::Overflow)?;
            value.1 = value
                .1
                .checked_add(
                    u64::from(occurrence.support)
                        .checked_mul(count)
                        .ok_or(SemanticError::Overflow)?,
                )
                .ok_or(SemanticError::Overflow)?;
        }
        let edges = supports
            .into_iter()
            .map(|(key, (multiplicity, support))| {
                let edge = Edge {
                    from: key.from,
                    to: key.to,
                    kind: key.kind,
                    multiplicity: u32::try_from(multiplicity)
                        .map_err(|_| SemanticError::Overflow)?,
                    support: u16::try_from(support).map_err(|_| SemanticError::Overflow)?,
                };
                Ok((key, edge))
            })
            .collect::<Result<Vec<_>, SemanticError>>()?;
        EdgeSet::from_sorted_edges(coverage, &edges)
    }

    /// Returns the coverage attached to the current occurrence state.
    #[must_use]
    pub const fn coverage(&self) -> FacetCoverage {
        self.coverage
    }

    /// Applies backend-flow weighted occurrence deltas.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::IncompleteNegativeFact`] for a negative delta
    /// without complete coverage, [`SemanticError::MissingSupport`] when a
    /// retraction exceeds retained support, or a corresponding validation
    /// error for conflicting, zero, or overflowing updates.
    pub fn apply_deltas(
        &mut self,
        coverage: FacetCoverage,
        deltas: &[FlowDelta<OccurrenceFact>],
    ) -> Result<EdgeSet, SemanticError> {
        if coverage.deletion() != crate::Deletion::Live && !deltas.is_empty() {
            return Err(SemanticError::InvalidCoverageState);
        }
        if deltas.iter().any(|delta| delta.diff.value() < 0) && !coverage.authoritative() {
            return Err(SemanticError::IncompleteNegativeFact);
        }
        if self.coverage.authoritative()
            && coverage.authoritative()
            && !self.coverage.same_scope(coverage)
        {
            return Err(SemanticError::ScopeMismatch);
        }

        let (pending, mut staged_work) = self.prepare_rows(deltas)?;
        let next_edges = self.prepare_edges(coverage, &pending, &mut staged_work)?;
        let (next_rows, row_count) = self.prepare_row_state(&pending, &mut staged_work)?;
        staged_work.checked_add_assign(self.work)?;
        let next_rows = next_rows.commit(&self.rows).map_err(map_row_delta_error)?;

        // Commit all prepared values only after all validation and checked
        // counter arithmetic has succeeded.
        self.rows = next_rows;
        self.row_count = row_count;
        self.coverage = coverage;
        self.edges = next_edges.clone();
        self.work = staged_work;
        Ok(next_edges)
    }

    fn prepare_rows(
        &self,
        deltas: &[FlowDelta<OccurrenceFact>],
    ) -> Result<(PendingRows, OccurrenceWorkCounters), SemanticError> {
        // Stage only touched addresses.  Until every row and derived edge has
        // passed validation, neither retained map is changed.
        let mut pending = PendingRows::new();
        let mut work = OccurrenceWorkCounters {
            input_rows: u64::try_from(deltas.len()).map_err(|_| SemanticError::Overflow)?,
            ..OccurrenceWorkCounters::default()
        };
        for delta in deltas {
            let occurrence = delta.value.clone();
            if occurrence.multiplicity == 0 || occurrence.support == 0 {
                return Err(SemanticError::InvalidSupport);
            }
            let address = occurrence.address();
            let amount = delta.diff.value();
            if amount == 0 {
                return Err(SemanticError::ZeroWeight);
            }
            let entry = match pending.entry(address.clone()) {
                std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::btree_map::Entry::Vacant(entry) => {
                    let retained = self.rows.get(&address);
                    work.occurrence_probes = work
                        .occurrence_probes
                        .checked_add(1)
                        .ok_or(SemanticError::Overflow)?;
                    entry.insert(retained.cloned().unwrap_or((occurrence.clone(), 0)))
                }
            };
            if amount > 0 {
                // A fully retracted row is no longer retained, so a later
                // positive update may establish a new value at its address.
                if entry.1 > 0 && entry.0 != occurrence {
                    return Err(SemanticError::ConflictingOccurrence);
                }
                if entry.1 == 0 {
                    entry.0 = occurrence;
                }
                entry.1 = entry.1.checked_add(amount).ok_or(SemanticError::Overflow)?;
            } else {
                if entry.1 == 0 {
                    return Err(SemanticError::MissingSupport);
                }
                if entry.0 != occurrence {
                    return Err(SemanticError::ConflictingOccurrence);
                }
                let next = entry.1.checked_add(amount).ok_or(SemanticError::Overflow)?;
                if next < 0 {
                    return Err(SemanticError::MissingSupport);
                }
                entry.1 = next;
            }
        }
        Ok((pending, work))
    }

    fn prepare_row_state(
        &self,
        pending: &PendingRows,
        work: &mut OccurrenceWorkCounters,
    ) -> Result<(backend_version::PreparedDelta<OccurrenceRows>, usize), SemanticError> {
        let mut changes = Vec::with_capacity(pending.len());
        let mut row_count = self.row_count;
        for (address, (next_occurrence, next_count)) in pending {
            let old = self.rows.get(address);
            work.occurrence_probes = work
                .occurrence_probes
                .checked_add(1)
                .ok_or(SemanticError::Overflow)?;
            let unchanged = match old {
                Some((occurrence, count)) => {
                    *next_count > 0 && *count == *next_count && occurrence == next_occurrence
                }
                None => *next_count == 0,
            };
            if unchanged {
                continue;
            }
            let before = old.cloned();
            let after = if *next_count == 0 {
                row_count = row_count.checked_sub(1).ok_or(SemanticError::Overflow)?;
                None
            } else {
                if before.is_none() {
                    row_count = row_count.checked_add(1).ok_or(SemanticError::Overflow)?;
                }
                Some((next_occurrence.clone(), *next_count))
            };
            changes.push(MapChange {
                key: address.clone(),
                before,
                after,
            });
        }
        let (prepared, delta_work) =
            prepare_delta_with_state(&self.rows, changes).map_err(map_row_delta_error)?;
        work.occurrence_probes = work
            .occurrence_probes
            .checked_add(
                u64::try_from(delta_work.visited_nodes).map_err(|_| SemanticError::Overflow)?,
            )
            .ok_or(SemanticError::Overflow)?;
        work.occurrence_nodes = work
            .occurrence_nodes
            .checked_add(u64::try_from(delta_work.nodes).map_err(|_| SemanticError::Overflow)?)
            .ok_or(SemanticError::Overflow)?;
        Ok((prepared, row_count))
    }

    fn prepare_edges(
        &self,
        coverage: FacetCoverage,
        pending: &PendingRows,
        work: &mut OccurrenceWorkCounters,
    ) -> Result<EdgeSet, SemanticError> {
        let mut edge_changes = BTreeMap::<EdgeKey, (i128, i128)>::new();
        for (address, (next_occurrence, next_count)) in pending {
            let old = self.rows.get(address);
            work.occurrence_probes = work
                .occurrence_probes
                .checked_add(1)
                .ok_or(SemanticError::Overflow)?;
            let unchanged = match old {
                Some((occurrence, count)) => {
                    *next_count > 0 && *count == *next_count && occurrence == next_occurrence
                }
                None => *next_count == 0,
            };
            if unchanged {
                continue;
            }
            work.occurrence_updates = work
                .occurrence_updates
                .checked_add(1)
                .ok_or(SemanticError::Overflow)?;
            if let Some((occurrence, count)) = old {
                Self::stage_edge_delta(&mut edge_changes, occurrence, -*count)?;
            }
            if *next_count > 0 {
                Self::stage_edge_delta(&mut edge_changes, next_occurrence, *next_count)?;
            }
        }

        // Resolve every affected edge against the old tree before publishing
        // a new tree, catching derived overflow/underflow without mutation.
        let mut resolved_edges = BTreeMap::<EdgeKey, Option<Edge>>::new();
        for (key, (multiplicity_delta, support_delta)) in edge_changes {
            let old = self.edges.tree.get(&key);
            work.edge_probes = work
                .edge_probes
                .checked_add(1)
                .ok_or(SemanticError::Overflow)?;
            let old_multiplicity = old.map_or(0_i128, |edge| i128::from(edge.multiplicity));
            let old_support = old.map_or(0_i128, |edge| i128::from(edge.support));
            let multiplicity = old_multiplicity
                .checked_add(multiplicity_delta)
                .ok_or(SemanticError::Overflow)?;
            let support = old_support
                .checked_add(support_delta)
                .ok_or(SemanticError::Overflow)?;
            if multiplicity < 0 || support < 0 {
                return Err(SemanticError::MissingSupport);
            }
            if multiplicity == 0 || support == 0 {
                if multiplicity != 0 || support != 0 {
                    return Err(SemanticError::MissingSupport);
                }
                if old.is_some() {
                    resolved_edges.insert(key, None);
                }
                continue;
            }
            let edge = Edge {
                from: key.from,
                to: key.to,
                kind: key.kind,
                multiplicity: u32::try_from(multiplicity).map_err(|_| SemanticError::Overflow)?,
                support: u16::try_from(support).map_err(|_| SemanticError::Overflow)?,
            };
            if old != Some(&edge) {
                resolved_edges.insert(key, Some(edge));
            }
        }
        work.affected_edges =
            u64::try_from(resolved_edges.len()).map_err(|_| SemanticError::Overflow)?;
        self.edges.apply_changes(coverage, &resolved_edges, work)
    }

    /// Returns the current supported edge set.
    ///
    /// # Errors
    ///
    /// The retained edge arrangement has already passed the schema bounds
    /// during replacement or the last successful update.
    pub fn edges(&self) -> Result<EdgeSet, SemanticError> {
        Ok(self.edges.clone())
    }

    fn stage_edge_delta(
        changes: &mut BTreeMap<EdgeKey, (i128, i128)>,
        occurrence: &OccurrenceFact,
        count: i64,
    ) -> Result<(), SemanticError> {
        let key = EdgeKey {
            from: occurrence.source,
            to: occurrence.target,
            kind: EdgeKind::Reference,
        };
        let entry = changes.entry(key).or_insert((0, 0));
        let count = i128::from(count);
        entry.0 = entry
            .0
            .checked_add(
                i128::from(occurrence.multiplicity)
                    .checked_mul(count)
                    .ok_or(SemanticError::Overflow)?,
            )
            .ok_or(SemanticError::Overflow)?;
        entry.1 = entry
            .1
            .checked_add(
                i128::from(occurrence.support)
                    .checked_mul(count)
                    .ok_or(SemanticError::Overflow)?,
            )
            .ok_or(SemanticError::Overflow)?;
        Ok(())
    }

    /// Returns the current weighted occurrence count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.row_count
    }

    /// Returns one retained occurrence and its weighted count.
    #[must_use]
    pub fn get(&self, address: &OccurrenceAddress) -> Option<(&OccurrenceFact, i64)> {
        self.rows
            .get(address)
            .map(|(occurrence, count)| (occurrence, *count))
    }

    /// Returns whether no occurrence support remains.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.row_count == 0
    }

    /// Returns measured structural work accumulated by successful batches.
    #[must_use]
    pub const fn work_counters(&self) -> OccurrenceWorkCounters {
        self.work
    }

    /// Returns the structural work counters under their descriptive alias.
    #[must_use]
    pub const fn structural_work(&self) -> OccurrenceWorkCounters {
        self.work
    }

    /// Returns the structural work counters through the short-form accessor.
    #[must_use]
    pub const fn work(&self) -> OccurrenceWorkCounters {
        self.work
    }

    /// Resets measured structural work counters without changing ledger state.
    pub const fn reset_work_counters(&mut self) {
        self.work = OccurrenceWorkCounters {
            input_rows: 0,
            occurrence_probes: 0,
            occurrence_updates: 0,
            affected_edges: 0,
            edge_probes: 0,
            edge_nodes: 0,
            occurrence_nodes: 0,
        };
    }

    #[cfg(test)]
    pub(crate) fn rows_root_is_shared_with(&self, other: &Self) -> bool {
        self.rows.root() == other.rows.root()
    }
}
