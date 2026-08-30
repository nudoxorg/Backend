use nudox_id::{Domain, GenerationId};
use nudox_object::{DepSetId, ObjectRef, ProviderSet};
use nudox_root::{GenerationEntry, Locality, LocalityReadError, MetadataBytes, SelectedOrdinals};

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

/// Borrowing, allocation-free hydration plan over explicit caller-owned scratch.
pub struct HydrationPlanView<'selection, 'storage, DomainTag> {
    /// Immutable root this exact plan was derived from.
    pub pinned_root: GenerationId,
    /// Exact projection represented by the plan.
    pub projection: Projection,
    /// Canonical identity of ordered required descriptors.
    pub dep_set: DepSetId,
    /// Exact aggregate coverage gathered while deriving this plan.
    pub coverage: PlanCoverage,
    absent: SelectedOrdinals<'selection, 'storage, DomainTag>,
}

impl<'selection, 'storage, DomainTag: Domain> HydrationPlanView<'selection, 'storage, DomainTag> {
    pub(super) const fn new(
        pinned_root: GenerationId,
        projection: Projection,
        dep_set: DepSetId,
        coverage: PlanCoverage,
        absent: SelectedOrdinals<'selection, 'storage, DomainTag>,
    ) -> Self {
        Self {
            pinned_root,
            projection,
            dep_set,
            coverage,
            absent,
        }
    }

    /// Iterates exact requested descriptors in canonical root order.
    pub fn required(
        &self,
    ) -> impl Iterator<Item = Result<ObjectRef<DomainTag>, LocalityReadError>> + '_ {
        self.absent
            .required_entries()
            .map(|entry| entry.map(|entry| entry.object))
    }

    /// Iterates descriptors already present under the planner predicate.
    pub fn present(
        &self,
    ) -> impl Iterator<Item = Result<ObjectRef<DomainTag>, LocalityReadError>> + '_ {
        self.absent
            .present_entries()
            .map(|entry| entry.map(|entry| entry.object))
    }

    /// Iterates promise-backed absent descriptors through borrowed locality.
    pub fn promised(
        &self,
    ) -> impl Iterator<Item = Result<Promise<DomainTag>, LocalityReadError>> + '_ {
        self.absent_entries().filter_map(|item| match item {
            Ok(entry) => match entry.locality {
                Locality::Promised(providers) => Some(Ok(Promise {
                    object: entry.object,
                    providers,
                })),
                Locality::Resident | Locality::Overlaid(_) => None,
            },
            Err(error) => Some(Err(error)),
        })
    }

    /// Iterates absent descriptors with no provider promise.
    pub fn missing(
        &self,
    ) -> impl Iterator<Item = Result<ObjectRef<DomainTag>, LocalityReadError>> + '_ {
        self.absent_entries().filter_map(|item| match item {
            Ok(entry) => matches!(entry.locality, Locality::Resident | Locality::Overlaid(_))
                .then_some(Ok(entry.object)),
            Err(error) => Some(Err(error)),
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
    pub fn fetches(
        &self,
    ) -> impl Iterator<Item = Result<Fetch<DomainTag>, LocalityReadError>> + '_ {
        self.absent_entries().map(|entry| {
            entry.map(|entry| match entry.locality {
                Locality::Promised(providers) => Fetch {
                    object: entry.object,
                    route: FetchRoute::Promised(providers),
                },
                Locality::Resident | Locality::Overlaid(_) => Fetch {
                    object: entry.object,
                    route: FetchRoute::Unrouted,
                },
            })
        })
    }

    /// Stages this borrowed plan; only verification can advance it to publication.
    #[must_use]
    pub const fn stage(&self) -> crate::StagedGeneration<'_, 'selection, 'storage, DomainTag> {
        crate::StagedGeneration::new(self)
    }

    fn absent_entries(
        &self,
    ) -> impl Iterator<Item = Result<GenerationEntry<DomainTag>, LocalityReadError>> + '_ {
        self.absent.absent_entries()
    }
}
