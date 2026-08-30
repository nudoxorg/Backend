mod evidence;
mod output;

use core::mem::size_of;

use nudox_id::{
    ContentHasher, DependencySetDomain, Domain, FixedCanonicalRecord, GenerationId, HASH_BYTES,
};
use nudox_object::{ObjectDescriptorWireRecord, ObjectRef};
use nudox_observe::Probe;
use nudox_root::{ClosureError, ClosureScratch, Locality};
use zerocopy::{
    Immutable, IntoBytes,
    byteorder::{BigEndian, U64},
};

use crate::{BoundBorrowedNeed, BoundNeed, Projection};

pub use evidence::{
    AbsentCount, PlanCoverage, PlanError, PlanRejection, PlanScratch, PlanScratchFacts,
};
pub use output::{BorrowedHydrationPlanView, Fetch, FetchRoute, HydrationPlanView, Promise};

/// Closed outcome for one completed hydration operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HydrationOutcome {
    /// Planning completed with exact aggregate coverage.
    Planned(PlanCoverage),
    /// Planning failed before a borrowing plan view was returned.
    Rejected(PlanRejection),
}

/// One completed hydration plan without roots, descriptors, or provider IDs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HydrationProbeEvent {
    /// Closed plan outcome.
    pub outcome: HydrationOutcome,
}

/// Pure wanted/have planner. No I/O, store mutation, retry, or hidden allocation occurs.
///
/// # Errors
///
/// Returns an exact closure-scratch or plan-scratch capacity failure before
/// returning a borrowing plan view.
pub fn plan<'scratch, 'view, 'root, 'locality, DomainTag: Domain, IsPresent>(
    need: BoundNeed<'view, 'root, 'locality, DomainTag>,
    closure_scratch: &'scratch mut ClosureScratch,
    plan_scratch: &'scratch mut PlanScratch,
    mut is_present: IsPresent,
) -> Result<HydrationPlanView<'scratch, 'scratch, DomainTag>, PlanError>
where
    IsPresent: FnMut(ObjectRef<DomainTag>) -> bool,
    'view: 'scratch,
    'root: 'scratch,
    'locality: 'scratch,
{
    let selected = need
        .view
        .select_closure(need.projection.range(), closure_scratch)
        .map_err(PlanError::Closure)?;
    let count = selected.count();
    if count > plan_scratch.capacity {
        return Err(PlanError::ScratchTooSmall {
            required: count,
            available: plan_scratch.capacity,
        });
    }

    let (ordinal_buffer, facts) = plan_scratch.begin_plan();
    ordinal_buffer.clear();
    let mut dependency_set = DependencySetWriter::new(need.view.id, need.projection, count);
    let mut tally = PlanTally::new(count);
    for (entry, ordinal) in selected.iter().zip(0_u32..) {
        let object = entry.object;
        dependency_set.descriptor(object);
        if is_present(object) {
            tally.record_present();
        } else {
            match entry.locality {
                Locality::Promised(_) => {
                    tally.record_promised();
                }
                Locality::Resident | Locality::Overlaid(_) => {
                    tally.record_missing();
                }
            }
            ordinal_buffer.push(ordinal);
        }
    }
    let (coverage, absent_count) = tally.finish();
    if absent_count > facts.high_water_absent {
        facts.high_water_absent = absent_count;
    }
    let positions = ordinal_buffer.as_slice();
    Ok(HydrationPlanView::new(
        need.view.id,
        need.projection,
        dependency_set.finish(),
        coverage,
        selected,
        positions,
    ))
}

/// Pure wanted/have planner over a borrowed canonical root/locality view.
/// No root bytes, descriptors, locality backing, or presence predicate are
/// retained by the returned plan; only the validated selected view and the
/// caller-owned sparse ordinal positions are borrowed.
///
/// # Errors
///
/// Returns the exact [`PlanError::Closure`] cause when closure selection cannot
/// use the caller-provided scratch, or [`PlanError::ScratchTooSmall`] with the
/// requested and available counts before changing sparse-plan accounting.
pub fn plan_borrowed<'scratch, 'view, 'root, 'locality, DomainTag: Domain, IsPresent>(
    need: BoundBorrowedNeed<'view, 'root, 'locality, DomainTag>,
    closure_scratch: &'scratch mut ClosureScratch,
    plan_scratch: &'scratch mut PlanScratch,
    mut is_present: IsPresent,
) -> Result<BorrowedHydrationPlanView<'scratch, 'scratch, 'root, 'locality, DomainTag>, PlanError>
where
    IsPresent: FnMut(ObjectRef<DomainTag>) -> bool,
    'view: 'scratch,
    'root: 'scratch,
    'locality: 'scratch,
{
    let selected = need
        .view
        .select_closure(need.projection.range(), closure_scratch)
        .map_err(PlanError::Closure)?;
    let count = selected.count();
    if count > plan_scratch.capacity {
        return Err(PlanError::ScratchTooSmall {
            required: count,
            available: plan_scratch.capacity,
        });
    }

    let (ordinal_buffer, facts) = plan_scratch.begin_plan();
    ordinal_buffer.clear();
    let mut dependency_set = DependencySetWriter::new(need.view.id, need.projection, count);
    let mut tally = PlanTally::new(count);
    for (entry, ordinal) in selected.iter().zip(0_u32..) {
        let object = entry.object;
        dependency_set.descriptor(object);
        if is_present(object) {
            tally.record_present();
        } else {
            match entry.locality {
                Locality::Promised(_) => {
                    tally.record_promised();
                }
                Locality::Resident | Locality::Overlaid(_) => {
                    tally.record_missing();
                }
            }
            ordinal_buffer.push(ordinal);
        }
    }
    let (coverage, absent_count) = tally.finish();
    if absent_count > facts.high_water_absent {
        facts.high_water_absent = absent_count;
    }
    let positions = ordinal_buffer.as_slice();
    Ok(BorrowedHydrationPlanView::new(
        need.view.id,
        need.projection,
        dependency_set.finish(),
        coverage,
        selected,
        positions,
    ))
}

/// Private selected-root-bounded count accumulator. Every increment is
/// guarded by the one `SelectedCount` supplied by the selected iterator; no
/// public semantic unit performs infallible arithmetic on arbitrary values.
struct PlanTally {
    required: nudox_root::SelectedCount,
    present: u32,
    promised: u32,
    missing: u32,
}

impl PlanTally {
    const fn new(selected: nudox_root::SelectedCount) -> Self {
        Self {
            required: selected,
            present: 0,
            promised: 0,
            missing: 0,
        }
    }

    const fn record_present(&mut self) {
        self.present += 1;
    }

    const fn record_promised(&mut self) {
        self.promised += 1;
    }

    const fn record_missing(&mut self) {
        self.missing += 1;
    }

    fn finish(self) -> (PlanCoverage, AbsentCount) {
        let absent = self.promised + self.missing;
        (
            PlanCoverage {
                required: self.required,
                present: self.present.into(),
                promised: self.promised.into(),
                missing: self.missing.into(),
            },
            absent.into(),
        )
    }
}

/// Derives one plan and lazily records its aggregate, closed outcome.
///
/// # Errors
///
/// Returns the exact planning rejection from [`plan`].
pub fn plan_with_probe<
    'scratch,
    'view,
    'root,
    'locality,
    DomainTag: Domain,
    IsPresent,
    Observation,
>(
    need: BoundNeed<'view, 'root, 'locality, DomainTag>,
    closure_scratch: &'scratch mut ClosureScratch,
    plan_scratch: &'scratch mut PlanScratch,
    is_present: IsPresent,
    probe: &mut Observation,
) -> Result<HydrationPlanView<'scratch, 'scratch, DomainTag>, PlanError>
where
    IsPresent: FnMut(ObjectRef<DomainTag>) -> bool,
    Observation: Probe<HydrationProbeEvent>,
    'view: 'scratch,
    'root: 'scratch,
    'locality: 'scratch,
{
    let result = plan(need, closure_scratch, plan_scratch, is_present);
    probe.record_with(|| HydrationProbeEvent {
        outcome: plan_outcome(&result),
    });
    result
}

const fn plan_outcome<DomainTag>(
    result: &Result<HydrationPlanView<'_, '_, DomainTag>, PlanError>,
) -> HydrationOutcome {
    match result {
        Ok(plan) => HydrationOutcome::Planned(plan.coverage),
        Err(PlanError::Closure(ClosureError::ScratchTooSmall { .. })) => {
            HydrationOutcome::Rejected(PlanRejection::ClosureScratchTooSmall)
        }
        Err(PlanError::ScratchTooSmall { .. }) => {
            HydrationOutcome::Rejected(PlanRejection::PlanScratchTooSmall)
        }
    }
}

/// Statefully writes the one canonical dependency-set preimage grammar.
/// Domain separation belongs to `DependencySetDomain`; this type owns only
/// ordered generation, projection, compact count, and descriptor fields.
struct DependencySetWriter {
    hasher: ContentHasher<DependencySetDomain>,
}

impl DependencySetWriter {
    fn new(
        pinned_root: GenerationId,
        projection: Projection,
        count: nudox_root::SelectedCount,
    ) -> Self {
        let mut hasher = ContentHasher::<DependencySetDomain>::new();
        match projection {
            Projection::CompleteGeneration => hasher.write_record(&CompleteDependencySetRecord {
                pinned_root: *pinned_root,
                projection: u8::from(projection.tag()),
                count: U64::new(u64::from(u32::from(count))),
            }),
            Projection::Range(range) => hasher.write_record(&RangeDependencySetRecord {
                pinned_root: *pinned_root,
                projection: u8::from(projection.tag()),
                start: U64::new(*range.start()),
                end: U64::new(*range.end()),
                count: U64::new(u64::from(u32::from(count))),
            }),
        }
        Self { hasher }
    }

    fn descriptor<DomainTag>(&mut self, object: ObjectRef<DomainTag>) {
        self.hasher
            .write_record(&ObjectDescriptorWireRecord::from(&object));
    }

    fn finish(self) -> nudox_object::DepSetId {
        self.hasher.finalize()
    }
}

#[repr(C)]
#[derive(Immutable, IntoBytes)]
struct CompleteDependencySetRecord {
    pinned_root: [u8; HASH_BYTES],
    projection: u8,
    count: U64<BigEndian>,
}

const COMPLETE_DEPENDENCY_RECORD_BYTES: usize = size_of::<CompleteDependencySetRecord>();

impl FixedCanonicalRecord<COMPLETE_DEPENDENCY_RECORD_BYTES> for CompleteDependencySetRecord {
    fn canonical_bytes(&self) -> &[u8; COMPLETE_DEPENDENCY_RECORD_BYTES] {
        zerocopy::transmute_ref!(self)
    }
}

#[repr(C)]
#[derive(Immutable, IntoBytes)]
struct RangeDependencySetRecord {
    pinned_root: [u8; HASH_BYTES],
    projection: u8,
    start: U64<BigEndian>,
    end: U64<BigEndian>,
    count: U64<BigEndian>,
}

const RANGE_DEPENDENCY_RECORD_BYTES: usize = size_of::<RangeDependencySetRecord>();

impl FixedCanonicalRecord<RANGE_DEPENDENCY_RECORD_BYTES> for RangeDependencySetRecord {
    fn canonical_bytes(&self) -> &[u8; RANGE_DEPENDENCY_RECORD_BYTES] {
        zerocopy::transmute_ref!(self)
    }
}
