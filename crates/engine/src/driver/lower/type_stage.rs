//! Staged anonymous and computed type rows.
//!
//! Declaration facts occupy `0..len`. Compound types that are not themselves
//! declarations live in two fixed lanes behind that range. A trailing child
//! run stays uncommitted until its row is interned, and an intern failure
//! rolls that run back so it cannot become the prefix of a later row.

use super::{
    ANONYMOUS_ROW_BASE, COMPUTED_ROW_BASE, FactSet, MAX_PENDING_TYPE_CHILDREN, ProjectionDemand,
    STAGED_TEXT_CHILD,
};
use crate::driver::types::{FactFault, TypeChildLane};
use backend_semantic::ir::{SemanticTypeChild, SemanticTypeRecord, TypeChildTarget};

/// One fixed child lane whose current trailing run has not yet been attached
/// to a committed type row.  The backing slot arrays stay reusable after an
/// abort; only their observable prefix/cursors are rolled back.
#[derive(Clone, Copy)]
enum PendingTypeLane {
    Anonymous,
    Computed,
}

impl<'source> FactSet<'source> {
    /// Abandons one uncommitted trailing child run while leaving its fixed
    /// storage available for the next row.  A row interning failure and a
    /// child append failure share this one transition so no stale child can
    /// become the prefix of a later otherwise-valid row.
    fn abandon_pending_type_run(&mut self, lane: PendingTypeLane) {
        let (total, pending) = match lane {
            PendingTypeLane::Anonymous => (
                &mut self.anonymous_children_total,
                &mut self.anonymous_child_pending,
            ),
            PendingTypeLane::Computed => (
                &mut self.computed_children_total,
                &mut self.computed_child_pending,
            ),
        };
        let pending_count = usize::from(*pending);
        debug_assert!(pending_count <= *total);
        *total -= pending_count;
        *pending = 0;
    }

    /// Preserves the precise fault that aborted a pending row after returning
    /// its child run to the reusable fixed lane.
    fn reject_pending_type_run<T>(
        &mut self,
        lane: PendingTypeLane,
        fault: FactFault,
    ) -> Result<T, FactFault> {
        self.abandon_pending_type_run(lane);
        Err(fault)
    }

    pub(super) fn staged_type_slot(&self, row: u32) -> Option<usize> {
        if row < ANONYMOUS_ROW_BASE {
            return ((row as usize) < self.len).then_some(row as usize);
        }
        if row >= COMPUTED_ROW_BASE {
            let computed = row - COMPUTED_ROW_BASE;
            return ((computed as usize) < self.computed_rows)
                .then_some(self.len + self.anonymous_rows + computed as usize);
        }
        let anonymous = row - ANONYMOUS_ROW_BASE;
        ((anonymous as usize) < self.anonymous_rows).then_some(self.len + anonymous as usize)
    }

    pub(super) fn is_anonymous_type_row(&self, row: u32) -> bool {
        row >= ANONYMOUS_ROW_BASE
            && row < COMPUTED_ROW_BASE
            && ((row - ANONYMOUS_ROW_BASE) as usize) < self.anonymous_rows
    }

    pub(super) fn is_computed_type_row(&self, row: u32) -> bool {
        row >= COMPUTED_ROW_BASE && ((row - COMPUTED_ROW_BASE) as usize) < self.computed_rows
    }

    /// Conservative active compound-projection bound for this transaction.
    ///
    /// A scratch lane is a depth-first stack, not a collection of unrelated
    /// row maxima: a parent prefix can remain live while one of its compound
    /// children is being completed.  Planning only the largest individual
    /// row therefore under-allocates nested tuples/objects.  This walk
    /// derives a safe maximum live path per typed scratch lane while retaining
    /// independent per-family allocations. Cache/order can make the observed
    /// peak smaller; this bound must never be smaller than that peak.
    pub(super) fn projected_type_demand(&self) -> ProjectionDemand {
        let row_count = self.len + self.anonymous_rows + self.computed_rows;
        let mut state = vec![0_u8; row_count].into_boxed_slice();
        let mut cached = vec![ProjectionDemand::default(); row_count].into_boxed_slice();
        let mut demand = ProjectionDemand::default();
        for ordinal in 0..self.len {
            demand = demand.maximum(self.projected_type_demand_from(
                ordinal as u32,
                &mut state,
                &mut cached,
            ));
        }
        for ordinal in 0..self.anonymous_rows {
            demand = demand.maximum(self.projected_type_demand_from(
                ANONYMOUS_ROW_BASE + ordinal as u32,
                &mut state,
                &mut cached,
            ));
        }
        for ordinal in 0..self.computed_rows {
            demand = demand.maximum(self.projected_type_demand_from(
                COMPUTED_ROW_BASE + ordinal as u32,
                &mut state,
                &mut cached,
            ));
        }
        demand
    }

    fn projected_type_demand_from(
        &self,
        row: u32,
        state: &mut [u8],
        cached: &mut [ProjectionDemand],
    ) -> ProjectionDemand {
        let Some(index) = self.staged_type_slot(row) else {
            return ProjectionDemand::default();
        };
        match state[index] {
            2 => return cached[index],
            // The owned projector later rejects this as `RecursiveType`.
            // Treating the back-edge as no additional scratch keeps planning
            // finite without turning a semantic fault into an allocation
            // policy.
            1 => return ProjectionDemand::default(),
            _ => {}
        }
        state[index] = 1;
        let record = self.type_record_for_demand(row);
        let child_count = self.type_child_count_for_demand(row);
        let mut deepest_child = ProjectionDemand::default();
        for position in 0..child_count {
            let Some((target, _, _)) = self.staged_type_child(row, position) else {
                continue;
            };
            if target != STAGED_TEXT_CHILD {
                deepest_child =
                    deepest_child.maximum(self.projected_type_demand_from(target, state, cached));
            }
        }
        let demand = ProjectionDemand::for_row(record.tag, child_count).with_child(deepest_child);
        state[index] = 2;
        cached[index] = demand;
        demand
    }

    fn type_record_for_demand(&self, row: u32) -> SemanticTypeRecord<'source> {
        debug_assert!(self.staged_type_slot(row).is_some());
        if row < ANONYMOUS_ROW_BASE {
            self.type_records[row as usize]
        } else if self.is_anonymous_type_row(row) {
            self.anonymous_records[(row - ANONYMOUS_ROW_BASE) as usize]
        } else {
            self.computed_records[(row - COMPUTED_ROW_BASE) as usize]
        }
    }

    fn type_child_count_for_demand(&self, row: u32) -> usize {
        debug_assert!(self.staged_type_slot(row).is_some());
        if row < ANONYMOUS_ROW_BASE {
            usize::from(self.type_child_counts[row as usize])
        } else if self.is_anonymous_type_row(row) {
            usize::from(self.anonymous_child_counts[(row - ANONYMOUS_ROW_BASE) as usize])
        } else {
            usize::from(self.computed_child_counts[(row - COMPUTED_ROW_BASE) as usize])
        }
    }

    /// Interns one anonymous type row owned by an already-pushed fact: the
    /// home of every compound type (applications, arrays, pointers,
    /// builtins) that is not itself a declaration. Children of the new row
    /// are appended with [`FactSet::anonymous_type_child`] before the next
    /// row is interned, keeping the pooled lane topologically backward.
    /// The returned coordinate is the row's type-lane ordinal; fact rows
    /// occupy `0..len`, anonymous rows follow.
    pub(super) fn intern_anonymous_type_row(
        &mut self,
        owner: u32,
        record: SemanticTypeRecord<'source>,
    ) -> Result<u32, FactFault> {
        if owner >= self.len as u32 {
            return self.reject_pending_type_run(
                PendingTypeLane::Anonymous,
                FactFault::RefTarget {
                    lane: backend_semantic::vocabulary::ProjectionFactLane::TypeRows,
                    raw: owner,
                    fact_count: self.len,
                },
            );
        }
        if self.anonymous_rows == self.plan.anonymous_rows {
            return self
                .reject_pending_type_run(PendingTypeLane::Anonymous, FactFault::TypeRowCapacity);
        }
        let child_count = u32::from(self.anonymous_child_pending);
        if let Err(fault) = record.validate(child_count).map_err(FactFault::TypeRecord) {
            return self.reject_pending_type_run(PendingTypeLane::Anonymous, fault);
        }
        if let Err(fault) = self.validate_pending_anonymous_children(record, child_count) {
            return self.reject_pending_type_run(PendingTypeLane::Anonymous, fault);
        }
        let row = ANONYMOUS_ROW_BASE + self.anonymous_rows as u32;
        let index = self.anonymous_rows;
        self.anonymous_records[index] = record;
        self.anonymous_owners[index] = owner;
        // The row's own children were appended before this intern (the go-lane
        // order): they occupy the trailing `child_count` slots, so the recorded
        // start is the range begin, not the post-append total.
        self.anonymous_child_starts[index] =
            (self.anonymous_children_total - child_count as usize) as u32;
        self.anonymous_child_counts[index] = child_count as u8;
        self.anonymous_rows += 1;
        self.anonymous_child_pending = 0;
        Ok(row)
    }

    /// Interns an anonymous row for a fact reserved immediately after the
    /// current fact prefix. The owner is admitted before that fact exists;
    /// the caller must push it next.
    pub(super) fn intern_reserved_anchor_type_row(
        &mut self,
        reserved_owner: u32,
        record: SemanticTypeRecord<'source>,
    ) -> Result<u32, FactFault> {
        if reserved_owner != self.len as u32 {
            return self.reject_pending_type_run(
                PendingTypeLane::Anonymous,
                FactFault::RefTarget {
                    lane: backend_semantic::vocabulary::ProjectionFactLane::ReservedTypeRows,
                    raw: reserved_owner,
                    fact_count: self.len,
                },
            );
        }
        debug_assert_eq!(reserved_owner, self.len as u32);
        if self.anonymous_rows == self.plan.anonymous_rows {
            return self
                .reject_pending_type_run(PendingTypeLane::Anonymous, FactFault::TypeRowCapacity);
        }
        let child_count = u32::from(self.anonymous_child_pending);
        if let Err(fault) = record.validate(child_count).map_err(FactFault::TypeRecord) {
            return self.reject_pending_type_run(PendingTypeLane::Anonymous, fault);
        }
        if let Err(fault) = self.validate_pending_anonymous_children(record, child_count) {
            return self.reject_pending_type_run(PendingTypeLane::Anonymous, fault);
        }
        let row = ANONYMOUS_ROW_BASE + self.anonymous_rows as u32;
        let index = self.anonymous_rows;
        self.anonymous_records[index] = record;
        self.anonymous_owners[index] = reserved_owner;
        self.anonymous_child_starts[index] =
            (self.anonymous_children_total - child_count as usize) as u32;
        self.anonymous_child_counts[index] = child_count as u8;
        self.anonymous_rows += 1;
        self.anonymous_child_pending = 0;
        Ok(row)
    }

    /// Appends one ordered child to the anonymous row currently being built.
    /// The target must name an already-interned anonymous row or an
    /// already-pushed fact; the lane rejects forward coordinates.
    pub(super) fn anonymous_type_child(
        &mut self,
        target: u32,
        name: Option<&'source [u8]>,
        flags: u8,
    ) -> Result<(), FactFault> {
        let valid = self.is_anonymous_type_row(target) || target < self.len as u32;
        if !valid {
            return self.reject_pending_type_run(
                PendingTypeLane::Anonymous,
                FactFault::TypeChildTarget {
                    position: usize::from(self.anonymous_child_pending),
                    target,
                    fact_count: self.len,
                },
            );
        }
        if self.anonymous_child_pending == MAX_PENDING_TYPE_CHILDREN {
            return self
                .reject_pending_type_run(PendingTypeLane::Anonymous, FactFault::TypeChildCapacity);
        }
        if self.anonymous_children_total == self.anonymous_child_targets.len() {
            let fault = FactFault::TypeChildPoolCapacity {
                lane: TypeChildLane::Anonymous,
                used: self.anonymous_children_total,
                requested: 1,
                capacity: self.anonymous_child_targets.len(),
            };
            return self.reject_pending_type_run(PendingTypeLane::Anonymous, fault);
        }
        let pooled = self.anonymous_children_total;
        self.anonymous_child_targets[pooled] = target;
        self.anonymous_child_names[pooled] = name;
        self.anonymous_child_flags[pooled] = flags;
        self.anonymous_children_total += 1;
        self.anonymous_child_pending += 1;
        Ok(())
    }

    /// Validates the pending anonymous child range as one closed row before
    /// committing it. This is where function result boundaries and a
    /// variadic final parameter are proven for anonymous/root callable types.
    fn validate_pending_anonymous_children(
        &self,
        record: SemanticTypeRecord<'source>,
        child_count: u32,
    ) -> Result<(), FactFault> {
        let child_start = self.anonymous_children_total - child_count as usize;
        for position in 0..child_count as usize {
            let pooled = child_start + position;
            record
                .validate_child_in_row(
                    position as u32,
                    child_count,
                    &SemanticTypeChild {
                        target: TypeChildTarget::Type(backend_semantic::ir::TypeRef::Local(
                            backend_semantic::ir::TypeId::new(self.anonymous_child_targets[pooled]),
                        )),
                        name: self.anonymous_child_names[pooled],
                        flags: self.anonymous_child_flags[pooled],
                    },
                )
                .map_err(|fault| FactFault::TypeChild { position, fault })?;
        }
        Ok(())
    }

    /// Interns one checker-computed row in the schema-2-only lane.
    pub(super) fn intern_computed_type_row(
        &mut self,
        owner: u32,
        record: SemanticTypeRecord<'source>,
    ) -> Result<u32, FactFault> {
        if owner >= self.len as u32 {
            return self.reject_pending_type_run(
                PendingTypeLane::Computed,
                FactFault::RefTarget {
                    lane: backend_semantic::vocabulary::ProjectionFactLane::ComputedOwners,
                    raw: owner,
                    fact_count: self.len,
                },
            );
        }
        if self.computed_rows == self.plan.computed_rows {
            return self.reject_pending_type_run(
                PendingTypeLane::Computed,
                FactFault::ComputedRowCapacity,
            );
        }
        let child_count = u32::from(self.computed_child_pending);
        if let Err(fault) = record.validate(child_count).map_err(FactFault::TypeRecord) {
            return self.reject_pending_type_run(PendingTypeLane::Computed, fault);
        }
        let child_start = self.computed_children_total - child_count as usize;
        for position in 0..child_count as usize {
            let pooled = child_start + position;
            let target = if self.computed_child_targets[pooled] == STAGED_TEXT_CHILD {
                TypeChildTarget::Text
            } else {
                TypeChildTarget::Type(backend_semantic::ir::TypeRef::Local(
                    backend_semantic::ir::TypeId::new(self.computed_child_targets[pooled]),
                ))
            };
            if let Err(fault) = record
                .validate_child_in_row(
                    position as u32,
                    child_count,
                    &SemanticTypeChild {
                        target,
                        name: self.computed_child_names[pooled],
                        flags: self.computed_child_flags[pooled],
                    },
                )
                .map_err(|fault| FactFault::TypeChild { position, fault })
            {
                return self.reject_pending_type_run(PendingTypeLane::Computed, fault);
            }
        }
        let index = self.computed_rows;
        self.computed_records[index] = record;
        self.computed_owners[index] = owner;
        self.computed_child_starts[index] =
            (self.computed_children_total - child_count as usize) as u32;
        self.computed_child_counts[index] = child_count as u8;
        self.computed_rows += 1;
        self.computed_child_pending = 0;
        Ok(COMPUTED_ROW_BASE + index as u32)
    }

    /// Appends a child to the computed row currently being built. Computed
    /// targets are either declared rows or strictly earlier computed rows.
    pub(super) fn computed_type_child(
        &mut self,
        target: u32,
        name: Option<&'source [u8]>,
        flags: u8,
    ) -> Result<(), FactFault> {
        let valid = target == STAGED_TEXT_CHILD
            || self.is_computed_type_row(target)
            || self.is_anonymous_type_row(target)
            || target < self.len as u32;
        if !valid {
            return self.reject_pending_type_run(
                PendingTypeLane::Computed,
                FactFault::TypeChildTarget {
                    position: usize::from(self.computed_child_pending),
                    target,
                    fact_count: self.len,
                },
            );
        }
        if self.computed_child_pending == MAX_PENDING_TYPE_CHILDREN {
            return self
                .reject_pending_type_run(PendingTypeLane::Computed, FactFault::TypeChildCapacity);
        }
        if self.computed_children_total == self.computed_child_targets.len() {
            let fault = FactFault::TypeChildPoolCapacity {
                lane: TypeChildLane::Computed,
                used: self.computed_children_total,
                requested: 1,
                capacity: self.computed_child_targets.len(),
            };
            return self.reject_pending_type_run(PendingTypeLane::Computed, fault);
        }
        let pooled = self.computed_children_total;
        self.computed_child_targets[pooled] = target;
        self.computed_child_names[pooled] = name;
        self.computed_child_flags[pooled] = flags;
        self.computed_children_total += 1;
        self.computed_child_pending += 1;
        Ok(())
    }

    /// Appends one literal template segment to the computed lane. It is kept
    /// distinct from a type coordinate all the way to `TypeChildTarget::Text`.
    pub(super) fn computed_type_text_child(
        &mut self,
        text: &'source [u8],
    ) -> Result<(), FactFault> {
        self.computed_type_child(STAGED_TEXT_CHILD, Some(text), 0)
    }

    /// First pooled position of one fact's type-record children.
    ///
    /// Admission records this prefix coordinate with the row. Recursive
    /// projection therefore performs one indexed lookup instead of summing
    /// every preceding row's child count for each visit.
    pub(super) fn type_children_base(&self, ordinal: usize) -> usize {
        self.type_child_starts[ordinal] as usize
    }

    /// One pooled type-record child of a fact row by absolute position.
    #[expect(
        clippy::type_complexity,
        reason = "the borrowed child triple is the pooled lane's own representation"
    )]
    pub(super) fn type_child_flat(&self, pooled: usize) -> (u32, Option<&'source [u8]>, u8) {
        (
            self.type_child_targets[pooled],
            self.type_child_names[pooled],
            self.type_child_flags[pooled],
        )
    }

    /// Returns one staged type row without exposing its storage plane.  The
    /// declared, anonymous, and observed-computed planes deliberately share
    /// this one projection boundary: consumers cannot accidentally treat a
    /// computed coordinate as an anonymous coordinate.
    pub(super) fn staged_type_record(
        &self,
        row: u32,
    ) -> Result<SemanticTypeRecord<'source>, backend_semantic::ir::BuildError> {
        if row < ANONYMOUS_ROW_BASE {
            return self.type_records.get(row as usize).copied().ok_or(
                backend_semantic::ir::BuildError::Dangling {
                    space: backend_semantic::ir::SemanticSpace::Type,
                    raw: row,
                },
            );
        }
        if self.is_anonymous_type_row(row) {
            return Ok(self.anonymous_records[(row - ANONYMOUS_ROW_BASE) as usize]);
        }
        if self.is_computed_type_row(row) {
            return Ok(self.computed_records[(row - COMPUTED_ROW_BASE) as usize]);
        }
        Err(backend_semantic::ir::BuildError::Dangling {
            space: backend_semantic::ir::SemanticSpace::Type,
            raw: row,
        })
    }

    /// Returns one staged child together with its exact text/tag payload.
    pub(super) fn staged_type_child(
        &self,
        row: u32,
        position: usize,
    ) -> Option<(u32, Option<&'source [u8]>, u8)> {
        if row < ANONYMOUS_ROW_BASE {
            let index = row as usize;
            return (position < usize::from(*self.type_child_counts.get(index)?))
                .then(|| self.type_child_flat(self.type_children_base(index) + position));
        }
        if self.is_anonymous_type_row(row) {
            let anonymous = (row - ANONYMOUS_ROW_BASE) as usize;
            return (position < usize::from(self.anonymous_child_counts[anonymous])).then(|| {
                let start = self.anonymous_child_starts[anonymous] as usize;
                (
                    self.anonymous_child_targets[start + position],
                    self.anonymous_child_names[start + position],
                    self.anonymous_child_flags[start + position],
                )
            });
        }
        if self.is_computed_type_row(row) {
            let computed = (row - COMPUTED_ROW_BASE) as usize;
            return (position < usize::from(self.computed_child_counts[computed])).then(|| {
                let start = self.computed_child_starts[computed] as usize;
                (
                    self.computed_child_targets[start + position],
                    self.computed_child_names[start + position],
                    self.computed_child_flags[start + position],
                )
            });
        }
        None
    }

    pub(super) fn staged_type_child_count(&self, row: u32) -> Option<usize> {
        if row < ANONYMOUS_ROW_BASE {
            return self
                .type_child_counts
                .get(row as usize)
                .copied()
                .map(usize::from);
        }
        if self.is_anonymous_type_row(row) {
            return Some(usize::from(
                self.anonymous_child_counts[(row - ANONYMOUS_ROW_BASE) as usize],
            ));
        }
        self.is_computed_type_row(row)
            .then(|| usize::from(self.computed_child_counts[(row - COMPUTED_ROW_BASE) as usize]))
    }

    /// Reports whether the committed row at `row` carries exactly the staged
    /// record cells of `record`. The pooled child span is staging geometry and
    /// compares through the targets instead.
    pub(super) fn staged_record_matches(
        &self,
        row: u32,
        record: &SemanticTypeRecord<'source>,
    ) -> bool {
        self.staged_type_slot(row)
            .is_some_and(|_| self.type_record_for_demand(row) == *record)
    }

    /// Reports whether two committed row coordinates carry structurally
    /// identical type frames: the record cells, the ordered staged children
    /// (names and flags), and every recursive target. Anonymous and computed
    /// staging mints fresh coordinates for genuinely identical graphs, so
    /// the comparison is structural, never ordinal-exact, below the roots.
    pub(super) fn staged_rows_structurally_equal(&self, left: u32, right: u32, depth: u8) -> bool {
        if left == right {
            return true;
        }
        if depth == 0 {
            return false;
        }
        if self.staged_type_slot(left).is_none() || self.staged_type_slot(right).is_none() {
            return left == right;
        }
        if !self.staged_record_matches(left, &self.type_record_for_demand(right)) {
            return false;
        }
        let Some(left_count) = self.staged_type_child_count(left) else {
            return left == right;
        };
        let Some(right_count) = self.staged_type_child_count(right) else {
            return left == right;
        };
        if left_count != right_count {
            return false;
        }
        for position in 0..left_count {
            let (
                Some((left_target, left_name, left_flags)),
                Some((right_target, right_name, right_flags)),
            ) = (
                self.staged_type_child(left, position),
                self.staged_type_child(right, position),
            )
            else {
                return false;
            };
            if left_name != right_name || left_flags != right_flags {
                return false;
            }
            if !self.staged_rows_structurally_equal(left_target, right_target, depth - 1) {
                return false;
            }
        }
        true
    }
}
