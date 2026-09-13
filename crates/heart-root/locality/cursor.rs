//! Defines locality cursor behavior for `heart-root`, whose purpose is to construct and validate immutable generation roots and locality metadata.
//! This module owns the locality cursor invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Forward-only sparse locality traversal over canonical root rows.

use backend_version::{Domain, GenerationId};
use backend_version::object::RemoteBase;

use super::{
    Locality, RowIndex, ValidatedLocality,
    artifact::{BorrowedLanes, LocalityDescriptorWireRecord, PlacementLanes, project_descriptor},
};

/// Work evidence emitted by one canonical locality scan.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LocalityScanWork {
    /// Canonical rows composed by this scan.
    pub rows: usize,
    /// Comparisons against the next sparse exception row.
    pub sparse_comparisons: usize,
}

/// Work evidence for one random composed locality lookup.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LocalityLookupWork {
    /// Number of sparse exception-set binary searches issued by the lookup.
    pub route_binary_searches: usize,
    /// Sparse-row comparisons made by those binary searches.
    pub route_comparisons: usize,
    /// Fixed membership words popcounted by random rank projections.
    pub rank_word_popcounts: u32,
}

/// Monotone sequential cursor. Named ordinals advance alongside sparse rows,
/// so scans perform no random rank query.
pub(crate) struct LocalityCursor<'locality, DomainTag> {
    locality: &'locality ValidatedLocality<'locality, DomainTag>,
    ordinals: CursorOrdinals,
}

#[derive(Clone, Copy)]
struct CursorOrdinals {
    exception: u32,
    promise: u32,
    overlay: u32,
    present: u32,
}

#[derive(Clone, Copy)]
enum ExceptionRoute<'lane> {
    Promise(&'lane super::artifact::ProviderWire),
    OverlayAbsent(GenerationId),
    OverlayPresent {
        basis: GenerationId,
        descriptor: &'lane LocalityDescriptorWireRecord,
    },
}

impl CursorOrdinals {
    const fn advance(&mut self, route: ExceptionRoute<'_>) {
        match route {
            ExceptionRoute::Promise(_) => self.promise += 1,
            ExceptionRoute::OverlayAbsent(_) => self.overlay += 1,
            ExceptionRoute::OverlayPresent { .. } => {
                self.overlay += 1;
                self.present += 1;
            }
        }
        self.exception += 1;
    }
}

impl<'locality, DomainTag: Domain> LocalityCursor<'locality, DomainTag> {
    pub(crate) const fn new(locality: &'locality ValidatedLocality<'locality, DomainTag>) -> Self {
        Self {
            locality,
            ordinals: CursorOrdinals {
                exception: 0,
                promise: 0,
                overlay: 0,
                present: 0,
            },
        }
    }

    pub(crate) fn locality_without_work(&mut self, row: RowIndex) -> Locality<DomainTag> {
        self.find(row, &mut ())
    }

    pub(super) fn locality_at(
        &mut self,
        row: RowIndex,
        work: &mut LocalityScanWork,
    ) -> Locality<DomainTag> {
        self.find(row, work)
    }

    fn find<Work: ScanWork>(&mut self, row: RowIndex, work: &mut Work) -> Locality<DomainTag> {
        while self.ordinals.exception < self.locality.exception_count {
            work.compared();
            match exception_row(self.locality.cursor_lanes(), self.ordinals.exception)
                .cmp(&row.compact)
            {
                core::cmp::Ordering::Less => self.skip_exception(),
                core::cmp::Ordering::Equal => return self.take_exception(),
                core::cmp::Ordering::Greater => return Locality::Resident,
            }
        }
        Locality::Resident
    }

    fn skip_exception(&mut self) {
        let route = route_at(self.locality.cursor_lanes(), self.ordinals);
        self.ordinals.advance(route);
    }

    fn take_exception(&mut self) -> Locality<DomainTag> {
        let route = route_at(self.locality.cursor_lanes(), self.ordinals);
        self.ordinals.advance(route);
        match route {
            ExceptionRoute::Promise(provider) => Locality::Promised(provider.provider_set()),
            ExceptionRoute::OverlayAbsent(generation) => {
                Locality::Overlaid(RemoteBase::Absent { generation })
            }
            ExceptionRoute::OverlayPresent { basis, descriptor } => {
                Locality::Overlaid(RemoteBase::Present {
                    generation: basis,
                    object: project_descriptor(descriptor, self.locality.content_authority),
                })
            }
        }
    }
}

trait ScanWork {
    fn compared(&mut self);
}

impl ScanWork for () {
    fn compared(&mut self) {}
}

impl ScanWork for LocalityScanWork {
    fn compared(&mut self) {
        self.sparse_comparisons += 1;
    }
}

#[allow(
    clippy::indexing_slicing,
    reason = "the cursor exception ordinal is bounded by the witness's exact typed row lane"
)]
fn exception_row(lanes: &BorrowedLanes<'_>, exception: u32) -> u32 {
    lanes.rows[native(exception)].get()
}

#[allow(
    clippy::indexing_slicing,
    reason = "validated populations and monotone cursor advancement prove each typed payload ordinal"
)]
fn route_at<'lanes>(
    lanes: &'lanes BorrowedLanes<'lanes>,
    ordinals: CursorOrdinals,
) -> ExceptionRoute<'lanes> {
    match &lanes.placement {
        PlacementLanes::PromisesOnly => {
            ExceptionRoute::Promise(&lanes.providers[native(ordinals.promise)])
        }
        PlacementLanes::Overlays(overlays) => {
            if super::artifact::rank_member(lanes.promise_bits, ordinals.exception) {
                return ExceptionRoute::Promise(&lanes.providers[native(ordinals.promise)]);
            }
            if super::artifact::rank_member(overlays.presence_bits, ordinals.overlay) {
                ExceptionRoute::OverlayPresent {
                    basis: overlays.basis,
                    descriptor: &overlays.descriptors[native(ordinals.present)],
                }
            } else {
                ExceptionRoute::OverlayAbsent(overlays.basis)
            }
        }
    }
}

#[allow(
    clippy::as_conversions,
    reason = "validated compact u32 ordinals fit the supported root address space"
)]
const fn native(ordinal: u32) -> usize {
    ordinal as usize
}
