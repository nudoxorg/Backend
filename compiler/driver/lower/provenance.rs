//! Transaction-local declaration provenance and containment state.
//!
//! This module owns the closed state transitions for facts whose truth comes
//! from authority topology rather than the declaration/type payload lanes.
//! The parent collector binds these facts to the owned semantic image after
//! all transitions have succeeded. Compact fragment bytes
//! are used only as a non-mutation control in this slice; durable provenance
//! serialization remains deliberately out of scope.

use compiler_ir::EntityId;

use crate::types::{FactFault, ParentageState, SourceSpanFact};

/// The staged primary-source fact used until projection attaches the
/// request-local source-file atom.
pub(crate) use crate::types::SourceSpanFact as StagedSourceSpan;

/// Whether the authority explicitly enumerated an entity's complete local
/// member set. The enum prevents root/bound containment from being mistaken
/// for proof of an empty or complete member list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MemberSetCapture {
    Unavailable,
    Captured,
}

/// Fixed, entity-aligned transaction state for declaration topology and
/// primary-source provenance.
pub(super) struct Provenance {
    parentage: Box<[ParentageState]>,
    source_spans: Box<[Option<SourceSpanFact>]>,
    member_sets: Box<[MemberSetCapture]>,
}

impl Provenance {
    /// Allocates the fixed state rows aligned to the caller's fact plan.
    pub(super) fn new(facts: usize) -> Self {
        Self {
            parentage: vec![ParentageState::Unavailable; facts].into_boxed_slice(),
            source_spans: vec![None; facts].into_boxed_slice(),
            member_sets: vec![MemberSetCapture::Unavailable; facts].into_boxed_slice(),
        }
    }

    /// Rows are exposed by borrow only so projections cannot mutate them
    /// without the closed transition methods below.
    pub(super) fn parentage(&self) -> &[ParentageState] {
        &self.parentage
    }

    /// Primary-source facts aligned to the fact lane.
    pub(super) fn source_spans(&self) -> &[Option<SourceSpanFact>] {
        &self.source_spans
    }

    /// Member-set authority state aligned to the fact lane.
    pub(super) fn member_sets(&self) -> &[MemberSetCapture] {
        &self.member_sets
    }

    /// Performs the one legal containment transition for an admitted entity.
    /// Identical authority observations are idempotent; all other second
    /// claims retain both closed states.
    fn transition_parentage(
        &mut self,
        entity: u32,
        requested: ParentageState,
    ) -> Result<(), FactFault> {
        let ordinal = entity as usize;
        let existing = self.parentage[ordinal];
        if existing == ParentageState::Unavailable {
            self.parentage[ordinal] = requested;
            return Ok(());
        }
        if existing == requested {
            return Ok(());
        }
        Err(FactFault::ConflictingParentage {
            entity: EntityId::new(entity),
            existing,
            requested,
        })
    }

    /// Binds a child to one already-admitted local parent.
    pub(super) fn attach_parent(
        &mut self,
        fact_count: usize,
        child: u32,
        parent: u32,
    ) -> Result<(), FactFault> {
        if child as usize >= fact_count || parent as usize >= fact_count || child == parent {
            return Err(FactFault::RefTarget {
                lane: "entity_parents",
                raw: if child as usize >= fact_count {
                    child
                } else {
                    parent
                },
                fact_count,
            });
        }
        self.transition_parentage(
            child,
            ParentageState::Bound {
                parent: EntityId::new(parent),
            },
        )
    }

    /// Records an authority-proven root relation.
    pub(super) fn mark_parentage_root(
        &mut self,
        fact_count: usize,
        entity: u32,
    ) -> Result<(), FactFault> {
        if entity as usize >= fact_count {
            return Err(FactFault::RefTarget {
                lane: "entity_parentage",
                raw: entity,
                fact_count,
            });
        }
        self.transition_parentage(entity, ParentageState::Root)
    }

    /// Retains an authority owner which has no local emitted row.
    pub(super) fn mark_unrepresented_parent(
        &mut self,
        fact_count: usize,
        entity: u32,
        authority_identity: [u8; 16],
    ) -> Result<(), FactFault> {
        if entity as usize >= fact_count {
            return Err(FactFault::RefTarget {
                lane: "entity_parentage",
                raw: entity,
                fact_count,
            });
        }
        self.transition_parentage(
            entity,
            ParentageState::UnrepresentedAuthorityOwner {
                identity: authority_identity,
            },
        )
    }

    /// Records one authority-bound declaration span. Identical repeated
    /// observations are idempotent; conflicting spans never overwrite truth.
    pub(super) fn attach_source_span(
        &mut self,
        fact_count: usize,
        primary_source_len: Option<u32>,
        entity: u32,
        span: SourceSpanFact,
    ) -> Result<(), FactFault> {
        if entity as usize >= fact_count {
            return Err(FactFault::RefTarget {
                lane: "entity_source_spans",
                raw: entity,
                fact_count,
            });
        }
        if let Some(source_len) = primary_source_len
            && span.end > source_len
        {
            return Err(FactFault::SourceSpan {
                entity,
                start: span.start,
                end: span.end,
                source_len,
            });
        }
        let slot = &mut self.source_spans[entity as usize];
        match *slot {
            None => {
                *slot = Some(span);
                Ok(())
            }
            Some(existing) if existing == span => Ok(()),
            Some(existing) => Err(FactFault::ConflictingSourceSpan {
                entity: EntityId::new(entity),
                existing,
                requested: span,
            }),
        }
    }

    /// Marks an authority-complete local member-set observation, including a
    /// real empty set. The binary capture state is intentionally idempotent.
    pub(super) fn mark_members_captured(
        &mut self,
        fact_count: usize,
        entity: u32,
    ) -> Result<(), FactFault> {
        if entity as usize >= fact_count {
            return Err(FactFault::RefTarget {
                lane: "entity_members",
                raw: entity,
                fact_count,
            });
        }
        self.member_sets[entity as usize] = MemberSetCapture::Captured;
        Ok(())
    }
}

/// Extracts the local parent representable by owned and compact projections.
/// Root, unavailable, and unrepresented native ownership never fabricate an
/// entity coordinate.
pub(super) const fn local_parent(parentage: ParentageState) -> Option<EntityId> {
    match parentage {
        ParentageState::Bound { parent } => Some(parent),
        ParentageState::Unavailable
        | ParentageState::Root
        | ParentageState::UnrepresentedAuthorityOwner { .. } => None,
    }
}
