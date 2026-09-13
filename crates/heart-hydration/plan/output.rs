//! Defines plan output behavior for `heart-hydration`, whose purpose is to plan and verify borrowed object hydration without weakening generation authority.
//! This module owns the plan output invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::{mem::size_of_val, ops::Deref};

use heart_identity::{Domain, GenerationId};
use heart_object::{DepSetId, ObjectRef, ProviderSet};
use heart_root::{
    BorrowedSelectedGeneration, GenerationEntry, Locality, MetadataBytes, SelectedGeneration,
};

use super::PlanCoverage;
use crate::Projection;

/// A promised absent object and the only providers eligible to obtain it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Promise<DomainTag> {
    /// Absent descriptor.
    pub object: ObjectRef<DomainTag>,
    /// Non-empty eligible provider set.
    pub providers: ProviderSet,
}

/// Closed route state for a required fetch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FetchRoute {
    /// The root names these eligible providers.
    Promised(ProviderSet),
    /// No provider promise exists; an adapter must choose a separate policy.
    Unrouted,
}

/// Exact deterministic fetch item derived from one absent descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Fetch<DomainTag> {
    /// Required immutable descriptor.
    pub object: ObjectRef<DomainTag>,
    /// Honest closed routing state.
    pub route: FetchRoute,
}

/// Read-only semantic facts shared by owned and borrowed hydration plans.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HydrationPlanFacts {
    /// Immutable root this exact plan was derived from.
    pub pinned_root: GenerationId,
    /// Exact projection represented by the plan.
    pub projection: Projection,
    /// Canonical identity of ordered required descriptors.
    pub dep_set: DepSetId,
    /// Exact aggregate coverage gathered while deriving this plan.
    pub coverage: PlanCoverage,
}

/// Borrowing, allocation-free hydration plan over explicit caller-owned scratch.
pub struct HydrationPlanView<'selection, 'storage, DomainTag> {
    pub(super) facts: HydrationPlanFacts,
    absent: OwnedSelectedOrdinals<'selection, 'storage, DomainTag>,
}

impl<DomainTag> Deref for HydrationPlanView<'_, '_, DomainTag> {
    type Target = HydrationPlanFacts;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

impl<'selection, 'storage, DomainTag: Domain> HydrationPlanView<'selection, 'storage, DomainTag> {
    pub(super) const fn new(
        pinned_root: GenerationId,
        projection: Projection,
        dep_set: DepSetId,
        coverage: PlanCoverage,
        selected: SelectedGeneration<'selection, DomainTag>,
        positions: &'storage [u32],
    ) -> Self {
        Self {
            facts: HydrationPlanFacts {
                pinned_root,
                projection,
                dep_set,
                coverage,
            },
            absent: OwnedSelectedOrdinals {
                selected,
                positions,
            },
        }
    }

    /// Iterates exact requested descriptors in canonical root order.
    pub fn required(&self) -> impl Iterator<Item = ObjectRef<DomainTag>> + '_ {
        self.absent.selected.iter().map(|entry| entry.object)
    }

    /// Iterates descriptors already present under the planner predicate.
    pub fn present(&self) -> impl Iterator<Item = ObjectRef<DomainTag>> + '_ {
        self.absent.present_entries().map(|entry| entry.object)
    }

    /// Iterates promise-backed absent descriptors through borrowed locality.
    pub fn promised(&self) -> impl Iterator<Item = Promise<DomainTag>> + '_ {
        self.absent_entries()
            .filter_map(|entry| match entry.locality {
                Locality::Promised(providers) => Some(Promise {
                    object: entry.object,
                    providers,
                }),
                Locality::Resident | Locality::Overlaid(_) => None,
            })
    }

    /// Iterates absent descriptors with no provider promise.
    pub fn missing(&self) -> impl Iterator<Item = ObjectRef<DomainTag>> + '_ {
        self.absent_entries().filter_map(|entry| {
            matches!(entry.locality, Locality::Resident | Locality::Overlaid(_))
                .then_some(entry.object)
        })
    }

    /// Returns whether every requested descriptor was present.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.coverage.required == self.coverage.present
    }

    /// Returns retained sparse plan-state bytes for this exact plan view.
    #[must_use]
    pub fn sparse_state_bytes(&self) -> MetadataBytes {
        self.absent.state_bytes().into()
    }

    /// Iterates exact fetch work in canonical root order, visiting only absent rows.
    pub fn fetches(&self) -> impl Iterator<Item = Fetch<DomainTag>> + '_ {
        self.absent_entries().map(|entry| match entry.locality {
            Locality::Promised(providers) => Fetch {
                object: entry.object,
                route: FetchRoute::Promised(providers),
            },
            Locality::Resident | Locality::Overlaid(_) => Fetch {
                object: entry.object,
                route: FetchRoute::Unrouted,
            },
        })
    }

    /// Stages this borrowed plan; only verification can advance it to publication.
    #[must_use]
    pub const fn stage(&self) -> crate::StagedGeneration<'_, 'selection, 'storage, DomainTag> {
        crate::StagedGeneration::new(self)
    }

    fn absent_entries(&self) -> impl Iterator<Item = GenerationEntry<DomainTag>> + '_ {
        self.absent.absent_entries()
    }
}

/// Borrowing sparse plan over a validated canonical root and locality map.
pub struct BorrowedHydrationPlanView<'selection, 'storage, 'root, 'locality, DomainTag> {
    facts: HydrationPlanFacts,
    absent: BorrowedSelectedOrdinals<'selection, 'storage, 'root, 'locality, DomainTag>,
}

impl<DomainTag> Deref for BorrowedHydrationPlanView<'_, '_, '_, '_, DomainTag> {
    type Target = HydrationPlanFacts;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

impl<'selection, 'storage, 'root, 'locality, DomainTag: Domain>
    BorrowedHydrationPlanView<'selection, 'storage, 'root, 'locality, DomainTag>
{
    pub(super) const fn new(
        pinned_root: GenerationId,
        projection: Projection,
        dep_set: DepSetId,
        coverage: PlanCoverage,
        selected: BorrowedSelectedGeneration<'selection, 'root, 'locality, DomainTag>,
        positions: &'storage [u32],
    ) -> Self {
        Self {
            facts: HydrationPlanFacts {
                pinned_root,
                projection,
                dep_set,
                coverage,
            },
            absent: BorrowedSelectedOrdinals {
                selected,
                positions,
            },
        }
    }

    /// Iterates exact requested descriptors in canonical root order.
    pub fn required(&self) -> impl Iterator<Item = ObjectRef<DomainTag>> + '_ {
        self.absent.selected.iter().map(|entry| entry.object)
    }

    /// Iterates descriptors already present under the planner predicate.
    pub fn present(&self) -> impl Iterator<Item = ObjectRef<DomainTag>> + '_ {
        self.absent.present_entries().map(|entry| entry.object)
    }

    /// Iterates promise-backed absent descriptors through borrowed locality.
    pub fn promised(&self) -> impl Iterator<Item = Promise<DomainTag>> + '_ {
        self.absent
            .absent_entries()
            .filter_map(|entry| match entry.locality {
                Locality::Promised(providers) => Some(Promise {
                    object: entry.object,
                    providers,
                }),
                Locality::Resident | Locality::Overlaid(_) => None,
            })
    }

    /// Iterates absent descriptors with no provider promise.
    pub fn missing(&self) -> impl Iterator<Item = ObjectRef<DomainTag>> + '_ {
        self.absent.absent_entries().filter_map(|entry| {
            matches!(entry.locality, Locality::Resident | Locality::Overlaid(_))
                .then_some(entry.object)
        })
    }

    /// Returns whether every requested descriptor was present.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.coverage.required == self.coverage.present
    }

    /// Returns retained sparse plan-state bytes for this exact plan view.
    #[must_use]
    pub fn sparse_state_bytes(&self) -> MetadataBytes {
        size_of_val(self.absent.positions).into()
    }

    /// Iterates exact fetch work in canonical root order, visiting only absent rows.
    pub fn fetches(&self) -> impl Iterator<Item = Fetch<DomainTag>> + '_ {
        self.absent
            .absent_entries()
            .map(|entry| match entry.locality {
                Locality::Promised(providers) => Fetch {
                    object: entry.object,
                    route: FetchRoute::Promised(providers),
                },
                Locality::Resident | Locality::Overlaid(_) => Fetch {
                    object: entry.object,
                    route: FetchRoute::Unrouted,
                },
            })
    }

    /// Stages this borrowed plan; only verification can advance it to publication.
    #[must_use]
    pub const fn stage(
        &self,
    ) -> crate::BorrowedStagedGeneration<'_, 'selection, 'storage, 'root, 'locality, DomainTag>
    {
        crate::BorrowedStagedGeneration::new(self)
    }
}

/// Sparse ordinals retained for an owned root selection.
struct OwnedSelectedOrdinals<'selection, 'storage, DomainTag> {
    selected: SelectedGeneration<'selection, DomainTag>,
    positions: &'storage [u32],
}

impl<DomainTag: Domain> OwnedSelectedOrdinals<'_, '_, DomainTag> {
    const fn state_bytes(&self) -> usize {
        size_of_val(self.positions)
    }

    fn present_entries(&self) -> impl Iterator<Item = GenerationEntry<DomainTag>> + '_ {
        let mut absent = self.positions.iter().copied().peekable();
        self.selected
            .iter()
            .zip(0_u32..)
            .filter_map(move |(entry, ordinal)| {
                if absent.peek().is_some_and(|position| *position == ordinal) {
                    absent.next();
                    None
                } else {
                    Some(entry)
                }
            })
    }

    fn absent_entries(&self) -> impl Iterator<Item = GenerationEntry<DomainTag>> + '_ {
        let mut positions = self.positions.iter().copied();
        let mut selected = self.selected.iter().zip(0_u32..);
        core::iter::from_fn(move || {
            let target = positions.next()?;
            selected.find_map(|(entry, ordinal)| (ordinal == target).then_some(entry))
        })
    }
}

/// Sparse ordinals retained for a borrowed canonical-root selection.
struct BorrowedSelectedOrdinals<'selection, 'storage, 'root, 'locality, DomainTag> {
    selected: BorrowedSelectedGeneration<'selection, 'root, 'locality, DomainTag>,
    positions: &'storage [u32],
}

impl<DomainTag: Domain> BorrowedSelectedOrdinals<'_, '_, '_, '_, DomainTag> {
    fn present_entries(&self) -> impl Iterator<Item = GenerationEntry<DomainTag>> + '_ {
        let mut absent = self.positions.iter().copied().peekable();
        self.selected
            .iter()
            .zip(0_u32..)
            .filter_map(move |(entry, ordinal)| {
                if absent.peek().is_some_and(|position| *position == ordinal) {
                    absent.next();
                    None
                } else {
                    Some(entry)
                }
            })
    }

    fn absent_entries(&self) -> impl Iterator<Item = GenerationEntry<DomainTag>> + '_ {
        let mut positions = self.positions.iter().copied();
        let mut selected = self.selected.iter().zip(0_u32..);
        core::iter::from_fn(move || {
            let target = positions.next()?;
            selected.find_map(|(entry, ordinal)| (ordinal == target).then_some(entry))
        })
    }
}
