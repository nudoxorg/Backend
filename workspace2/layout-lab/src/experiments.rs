//! Deterministic candidate suites.  Counters describe logical accesses; they are not hardware
//! performance-counter claims.  The target and unavailable counter tools are recorded by scripts.

use core::{
    alloc::{GlobalAlloc, Layout},
    cell::UnsafeCell,
    mem::{MaybeUninit, size_of},
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    task::Waker,
};
use std::{
    collections::{BTreeSet, VecDeque},
    sync::Mutex,
};

#[cfg(test)]
use std::sync::Arc;

use arrayvec::ArrayVec;
use atomic_waker::AtomicWaker;
use crossbeam_queue::ArrayQueue;
use nudox_id::{CapabilityDomain, ConfigurationDomain, ContentId, GenerationId, ObjectDomain};
use nudox_object::{ProviderId, ProviderSet};
use nudox_schema::OperationId;
use nudox_store_memory::{InlineMemoryStore, MemoryStore, StoreCapacity};
use nudox_workflow::{
    EventKind, MemoryWorkflowLog, ReductionError, StageId, StageInput, StageKey, WorkflowEvent,
    WorkflowState, WorkflowVersion, reduce,
};

const CACHE_LINE_BYTES: usize = 64;
const ACTIVE: u64 = 1;
const FREE: u64 = 0;
const READY: u64 = 2;
const CLAIMED: u64 = 3;
const CANCELLED: u64 = 4;
const RETIRED: u64 = 5;
const STATUS_MASK: u64 = 7;
const STATUS_BITS: u64 = 3;

mod locality;

/// An independent scoped allocation counter for this non-production binary.
///
/// It deliberately supports one measurement at a time.  Each measured closure creates and drops
/// all of its allocations before leaving the scope; this lets `Layout::size()` account peak live
/// requested bytes without pretending to measure allocator metadata or RSS.
pub struct TrackingAllocator {
    enabled: AtomicBool,
    allocations: AtomicUsize,
    deallocations: AtomicUsize,
    allocated_bytes: AtomicUsize,
    live_bytes: AtomicUsize,
    peak_live_bytes: AtomicUsize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AllocationFacts {
    pub allocations: usize,
    pub deallocations: usize,
    pub allocated_bytes: usize,
    pub peak_live_bytes: usize,
}

impl TrackingAllocator {
    pub const fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            allocations: AtomicUsize::new(0),
            deallocations: AtomicUsize::new(0),
            allocated_bytes: AtomicUsize::new(0),
            live_bytes: AtomicUsize::new(0),
            peak_live_bytes: AtomicUsize::new(0),
        }
    }

    /// Runs one allocation-isolated closure. It is intentionally not used from parallel tests.
    pub fn measure<Result>(&self, operation: impl FnOnce() -> Result) -> (Result, AllocationFacts) {
        assert!(
            !self.enabled.swap(true, Ordering::SeqCst),
            "layout-lab allocation scopes must not overlap"
        );
        self.allocations.store(0, Ordering::Relaxed);
        self.deallocations.store(0, Ordering::Relaxed);
        self.allocated_bytes.store(0, Ordering::Relaxed);
        self.live_bytes.store(0, Ordering::Relaxed);
        self.peak_live_bytes.store(0, Ordering::Relaxed);
        let result = operation();
        self.enabled.store(false, Ordering::SeqCst);
        (
            result,
            AllocationFacts {
                allocations: self.allocations.load(Ordering::Relaxed),
                deallocations: self.deallocations.load(Ordering::Relaxed),
                allocated_bytes: self.allocated_bytes.load(Ordering::Relaxed),
                peak_live_bytes: self.peak_live_bytes.load(Ordering::Relaxed),
            },
        )
    }

    fn allocate(&self, bytes: usize) {
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }
        self.allocations.fetch_add(1, Ordering::Relaxed);
        self.allocated_bytes.fetch_add(bytes, Ordering::Relaxed);
        let current = self.live_bytes.fetch_add(bytes, Ordering::Relaxed) + bytes;
        let mut observed = self.peak_live_bytes.load(Ordering::Relaxed);
        while current > observed {
            match self.peak_live_bytes.compare_exchange_weak(
                observed,
                current,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(next) => observed = next,
            }
        }
    }

    fn deallocate(&self, bytes: usize) {
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }
        self.deallocations.fetch_add(1, Ordering::Relaxed);
        let _prior = self.live_bytes.fetch_sub(bytes, Ordering::Relaxed);
    }
}
impl Default for TrackingAllocator {
    fn default() -> Self {
        Self::new()
    }
}

// SAFETY: this wrapper forwards every valid layout to `System` unchanged. Counters contain no
// pointers and are relaxed observational metadata; they do not affect allocation ownership.
unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: `layout` is supplied by the caller of the `GlobalAlloc` contract.
        let pointer = unsafe { std::alloc::System.alloc(layout) };
        if !pointer.is_null() {
            self.allocate(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: `layout` is supplied by the caller of the `GlobalAlloc` contract.
        let pointer = unsafe { std::alloc::System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            self.allocate(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        self.deallocate(layout.size());
        // SAFETY: `pointer` and `layout` are supplied by the caller of the `GlobalAlloc` contract.
        unsafe { std::alloc::System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `pointer` and `layout` are supplied by the caller of the `GlobalAlloc` contract.
        let replacement = unsafe { std::alloc::System.realloc(pointer, layout, new_size) };
        if !replacement.is_null() {
            self.deallocate(layout.size());
            self.allocate(new_size);
        }
        replacement
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Coverage {
    Present,
    Promised(ProviderSet),
    Missing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CoverageTag {
    Present,
    Promised,
    Missing,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct AccessFacts {
    visited_rows: usize,
    branches: usize,
    atomics: usize,
    cache_lines: usize,
}

struct DenseCoverage {
    states: Vec<Coverage>,
}

struct SparseCoverage {
    /// Ordinals of absent rows only; promised providers remain borrowed from root locality facts.
    absent_ordinals: Vec<u32>,
}

struct BitsetCoverage {
    /// One absent bit per selected row; provider facts remain borrowed from root locality facts.
    absent: Vec<u64>,
}

fn provider() -> Result<ProviderSet, nudox_object::ProviderIdError> {
    ProviderId::try_from(7_u8).map(ProviderSet::only)
}

fn coverage_input(
    rows: usize,
    absent_percent: usize,
) -> Result<Vec<Option<ProviderSet>>, nudox_object::ProviderIdError> {
    let absent_rows = rows.saturating_mul(absent_percent) / 100;
    (0..rows)
        .map(
            |row| -> Result<Option<ProviderSet>, nudox_object::ProviderIdError> {
                let absent = row < absent_rows;
                if absent && row % 2 == 0 {
                    Ok(Some(provider()?))
                } else if absent {
                    Ok(None)
                } else {
                    Ok(Some(ProviderId::try_from(1_u8).map(ProviderSet::only)?))
                }
            },
        )
        .collect::<Result<Vec<_>, _>>()
}

fn is_absent(route: Option<ProviderSet>, row: usize, absent_rows: usize) -> bool {
    let _route = route;
    row < absent_rows
}

fn dense_build(routes: &[Option<ProviderSet>], absent_percent: usize) -> DenseCoverage {
    let absent_rows = routes.len().saturating_mul(absent_percent) / 100;
    DenseCoverage {
        states: routes
            .iter()
            .enumerate()
            .map(|(row, route)| {
                if !is_absent(*route, row, absent_rows) {
                    Coverage::Present
                } else {
                    route.map_or(Coverage::Missing, Coverage::Promised)
                }
            })
            .collect(),
    }
}

fn sparse_build(routes: &[Option<ProviderSet>], absent_percent: usize) -> SparseCoverage {
    let absent_rows = routes.len().saturating_mul(absent_percent) / 100;
    let absent_ordinals = routes
        .iter()
        .enumerate()
        .filter_map(|(row, route)| is_absent(*route, row, absent_rows).then_some(row as u32))
        .collect();
    SparseCoverage { absent_ordinals }
}

fn bitset_build(routes: &[Option<ProviderSet>], absent_percent: usize) -> BitsetCoverage {
    let absent_rows = routes.len().saturating_mul(absent_percent) / 100;
    let mut absent = vec![0_u64; routes.len().div_ceil(64)];
    for (row, route) in routes.iter().enumerate() {
        if is_absent(*route, row, absent_rows) {
            absent[row / 64] |= 1_u64 << (row % 64);
        }
    }
    BitsetCoverage { absent }
}

fn lines(base: usize, stride: usize, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    let mut set = BTreeSet::new();
    for index in 0..count {
        set.insert((base + index * stride) / CACHE_LINE_BYTES);
    }
    set.len()
}

impl DenseCoverage {
    fn enumerate(&self) -> (Vec<CoverageTag>, AccessFacts) {
        let mut output = Vec::with_capacity(self.states.len());
        let mut facts = AccessFacts {
            cache_lines: lines(0, size_of::<Coverage>(), self.states.len()),
            ..AccessFacts::default()
        };
        for state in &self.states {
            facts.visited_rows += 1;
            facts.branches += 1;
            output.push(match state {
                Coverage::Present => CoverageTag::Present,
                Coverage::Promised(_) => CoverageTag::Promised,
                Coverage::Missing => CoverageTag::Missing,
            });
        }
        (output, facts)
    }
    fn retained_bytes(&self) -> usize {
        self.states.capacity() * size_of::<Coverage>()
    }
}

impl SparseCoverage {
    fn enumerate(&self, routes: &[Option<ProviderSet>]) -> (Vec<CoverageTag>, AccessFacts) {
        let mut output = Vec::with_capacity(routes.len());
        let mut cursor = 0;
        let mut facts = AccessFacts {
            cache_lines: lines(0, size_of::<u32>(), self.absent_ordinals.len()),
            ..AccessFacts::default()
        };
        for (row, route) in routes.iter().enumerate() {
            facts.visited_rows += 1;
            facts.branches += 1;
            let absent = self.absent_ordinals.get(cursor).copied() == Some(row as u32);
            if absent {
                cursor += 1;
            }
            output.push(if absent {
                route.map_or(CoverageTag::Missing, |_| CoverageTag::Promised)
            } else {
                CoverageTag::Present
            });
        }
        (output, facts)
    }
    fn retained_bytes(&self) -> usize {
        self.absent_ordinals.capacity() * size_of::<u32>()
    }
}

impl BitsetCoverage {
    fn enumerate(&self, routes: &[Option<ProviderSet>]) -> (Vec<CoverageTag>, AccessFacts) {
        let mut output = Vec::with_capacity(routes.len());
        let mut facts = AccessFacts {
            cache_lines: lines(0, size_of::<u64>(), self.absent.len()),
            ..AccessFacts::default()
        };
        for (row, route) in routes.iter().enumerate() {
            facts.visited_rows += 1;
            facts.branches += 1;
            let absent = self.absent[row / 64] & (1_u64 << (row % 64)) != 0;
            output.push(if absent {
                route.map_or(CoverageTag::Missing, |_| CoverageTag::Promised)
            } else {
                CoverageTag::Present
            });
        }
        (output, facts)
    }
    fn retained_bytes(&self) -> usize {
        self.absent.capacity() * size_of::<u64>()
    }
}

fn noop_waker() -> &'static Waker {
    Waker::noop()
}

struct DenseWaiter {
    state: AtomicU64,
    credits: AtomicUsize,
    waker: AtomicWaker,
}
impl DenseWaiter {
    fn new() -> Self {
        Self {
            state: AtomicU64::new(FREE),
            credits: AtomicUsize::new(0),
            waker: AtomicWaker::new(),
        }
    }
}

struct DenseWaiters {
    slots: Vec<DenseWaiter>,
}
struct SoaWaiters {
    states: Vec<AtomicU64>,
    credits: Vec<AtomicUsize>,
    wakers: Vec<AtomicWaker>,
}
struct BitmapWaiters {
    active: AtomicU64,
    slots: Vec<DenseWaiter>,
}
#[derive(Clone, Copy)]
struct BucketTicket {
    index: usize,
    epoch: u64,
}
struct BucketWaiters {
    slots: Vec<DenseWaiter>,
    buckets: [Vec<BucketTicket>; 4],
}

fn next_random(mut value: u64) -> u64 {
    value = value
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    value
}
fn active_positions(active: usize, capacity: usize) -> Vec<usize> {
    let mut positions = BTreeSet::new();
    let mut seed = 0x5eed_u64;
    while positions.len() < active {
        seed = next_random(seed);
        positions.insert((seed as usize) % capacity);
    }
    positions.into_iter().collect()
}
fn activate_slot(slot: &DenseWaiter, credits: usize) {
    slot.credits.store(credits, Ordering::Release);
    slot.waker.register(noop_waker());
    slot.state.store(ACTIVE, Ordering::Release);
}
fn line_count_for_indices(stride: usize, indices: impl Iterator<Item = usize>) -> usize {
    indices
        .map(|index| (index * stride) / CACHE_LINE_BYTES)
        .collect::<BTreeSet<_>>()
        .len()
}

impl DenseWaiters {
    fn new(capacity: usize, active: &[usize]) -> Self {
        let slots = (0..capacity)
            .map(|_| DenseWaiter::new())
            .collect::<Vec<_>>();
        let this = Self { slots };
        for &index in active {
            activate_slot(&this.slots[index], 1);
        }
        this
    }
    fn wake(&self, bytes: usize) -> AccessFacts {
        let mut facts = AccessFacts::default();
        let mut touched = BTreeSet::new();
        for (index, slot) in self.slots.iter().enumerate() {
            facts.visited_rows += 1;
            facts.branches += 1;
            facts.atomics += 1; // state load; credit is short-circuited for inactive slots.
            touched.insert((index * size_of::<DenseWaiter>()) / CACHE_LINE_BYTES);
            if slot.state.load(Ordering::Acquire) == ACTIVE {
                facts.atomics += 1;
                facts.branches += 1;
                if slot.credits.load(Ordering::Acquire) <= bytes {
                    slot.waker.wake();
                    // AtomicWaker's internal implementation is opaque; do not invent its count.
                }
            }
        }
        facts.cache_lines = touched.len();
        facts
    }
}

impl SoaWaiters {
    fn new(capacity: usize, active: &[usize]) -> Self {
        let states = (0..capacity).map(|_| AtomicU64::new(FREE)).collect();
        let credits = (0..capacity).map(|_| AtomicUsize::new(0)).collect();
        let wakers = (0..capacity).map(|_| AtomicWaker::new()).collect();
        let this = Self {
            states,
            credits,
            wakers,
        };
        for &index in active {
            this.credits[index].store(1, Ordering::Release);
            this.wakers[index].register(noop_waker());
            this.states[index].store(ACTIVE, Ordering::Release);
        }
        this
    }
    fn wake(&self, bytes: usize) -> AccessFacts {
        let mut facts = AccessFacts {
            cache_lines: line_count_for_indices(size_of::<AtomicU64>(), 0..self.states.len()),
            ..AccessFacts::default()
        };
        for index in 0..self.states.len() {
            facts.visited_rows += 1;
            facts.branches += 1;
            facts.atomics += 1;
            if self.states[index].load(Ordering::Acquire) == ACTIVE {
                facts.atomics += 1;
                facts.branches += 1;
                if self.credits[index].load(Ordering::Acquire) <= bytes {
                    self.wakers[index].wake();
                    facts.cache_lines +=
                        line_count_for_indices(size_of::<AtomicWaker>(), core::iter::once(index));
                }
                facts.cache_lines +=
                    line_count_for_indices(size_of::<AtomicUsize>(), core::iter::once(index));
            }
        }
        facts
    }
}

impl BitmapWaiters {
    fn new(capacity: usize, active: &[usize]) -> Self {
        assert!(capacity <= 64);
        let slots = (0..capacity)
            .map(|_| DenseWaiter::new())
            .collect::<Vec<_>>();
        let this = Self {
            active: AtomicU64::new(0),
            slots,
        };
        for &index in active {
            activate_slot(&this.slots[index], 1);
            this.active.fetch_or(1_u64 << index, Ordering::Release);
        }
        this
    }
    fn cancel(&self, index: usize) {
        self.slots[index].state.store(FREE, Ordering::Release);
        self.active.fetch_and(!(1_u64 << index), Ordering::AcqRel);
    }
    fn rearm(&self, index: usize) {
        activate_slot(&self.slots[index], 1);
        self.active.fetch_or(1_u64 << index, Ordering::Release);
    }
    fn wake_snapshot(&self, mut bitmap: u64, bytes: usize) -> AccessFacts {
        let mut facts = AccessFacts {
            atomics: 1,
            cache_lines: 1,
            ..AccessFacts::default()
        };
        while bitmap != 0 {
            let index = bitmap.trailing_zeros() as usize;
            bitmap &= bitmap - 1;
            facts.visited_rows += 1;
            facts.branches += 2;
            facts.atomics += 2;
            if self.slots[index].state.load(Ordering::Acquire) == ACTIVE
                && self.slots[index].credits.load(Ordering::Acquire) <= bytes
            {
                self.slots[index].waker.wake();
                facts.atomics += 1;
            }
        }
        facts
    }
    fn wake(&self, bytes: usize) -> AccessFacts {
        self.wake_snapshot(self.active.load(Ordering::Acquire), bytes)
    }
}

impl BucketWaiters {
    fn new(capacity: usize, active: &[usize]) -> Self {
        let slots = (0..capacity)
            .map(|_| DenseWaiter::new())
            .collect::<Vec<_>>();
        let mut this = Self {
            slots,
            buckets: core::array::from_fn(|_| Vec::new()),
        };
        for &index in active {
            activate_slot(&this.slots[index], 1);
            this.buckets[0].push(BucketTicket { index, epoch: 1 });
        }
        this
    }
    fn wake(&self, bytes: usize) -> AccessFacts {
        let mut facts = AccessFacts::default();
        for (bucket, tickets) in self.buckets.iter().enumerate() {
            if bucket > bytes.min(3) {
                continue;
            }
            for ticket in tickets {
                let _epoch = ticket.epoch;
                facts.visited_rows += 1;
                facts.branches += 3;
                facts.atomics += 2;
                if self.slots[ticket.index].state.load(Ordering::Acquire) == ACTIVE
                    && self.slots[ticket.index].credits.load(Ordering::Acquire) <= bytes
                {
                    self.slots[ticket.index].waker.wake();
                    facts.atomics += 1;
                }
            }
        }
        facts.cache_lines = line_count_for_indices(
            size_of::<DenseWaiter>(),
            self.buckets.iter().flatten().map(|ticket| ticket.index),
        );
        facts
    }
}

// -------------------------------------------------------------------------------------------------
// Flight recorder alternatives

struct OptionRing<Event, const CAP: usize> {
    slots: [Option<Event>; CAP],
    head: usize,
    len: usize,
}
impl<Event, const CAP: usize> OptionRing<Event, CAP> {
    fn new() -> Self {
        Self {
            slots: [const { None }; CAP],
            head: 0,
            len: 0,
        }
    }
    fn record(&mut self, event: Event) {
        if CAP == 0 {
            drop(event);
            return;
        }
        if self.len < CAP {
            let tail = (self.head + self.len) % CAP;
            self.slots[tail] = Some(event);
            self.len += 1;
        } else {
            self.slots[self.head] = Some(event);
            self.head = (self.head + 1) % CAP;
        }
    }
    fn events(&self) -> impl Iterator<Item = &Event> {
        (0..self.len).filter_map(move |offset| self.slots[(self.head + offset) % CAP].as_ref())
    }
}

struct ArrayVecRing<Event, const CAP: usize> {
    slots: ArrayVec<Event, CAP>,
    head: usize,
}
impl<Event, const CAP: usize> ArrayVecRing<Event, CAP> {
    fn new() -> Self {
        Self {
            slots: ArrayVec::new(),
            head: 0,
        }
    }
    fn record(&mut self, event: Event) {
        if CAP == 0 {
            drop(event);
            return;
        }
        if self.slots.len() < CAP {
            if let Err(error) = self.slots.try_push(event) {
                drop(error.element());
            }
        } else {
            self.slots[self.head] = event;
            self.head = (self.head + 1) % CAP;
        }
    }
    fn events(&self) -> impl Iterator<Item = &Event> {
        let len = self.slots.len();
        (0..len).map(move |offset| &self.slots[(self.head + offset) % CAP])
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Event1([u8; 1]);
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Event8([u8; 8]);
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Event68([u8; 68]);
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Event128([u8; 128]);

// -------------------------------------------------------------------------------------------------
// Workflow replay representation experiment

#[derive(Clone, Copy)]
enum AcceptedTag {
    Requested,
    Admitted,
    Staged,
    Verified,
    PublicationStarted,
    Published,
    Failed(nudox_workflow::FailureCode),
    Cancelled,
}
impl AcceptedTag {
    fn from_event(kind: EventKind) -> Self {
        match kind {
            EventKind::Requested => Self::Requested,
            EventKind::Admitted => Self::Admitted,
            EventKind::Staged { .. } => Self::Staged,
            EventKind::Verified { .. } => Self::Verified,
            EventKind::PublicationStarted { .. } => Self::PublicationStarted,
            EventKind::Published { .. } => Self::Published,
            EventKind::Failed { code } => Self::Failed(code),
            EventKind::Cancelled => Self::Cancelled,
        }
    }
    fn event(self, output: Option<ContentId<ObjectDomain>>) -> Option<EventKind> {
        match self {
            Self::Requested => Some(EventKind::Requested),
            Self::Admitted => Some(EventKind::Admitted),
            Self::Staged => output.map(|output| EventKind::Staged { output }),
            Self::Verified => output.map(|output| EventKind::Verified { output }),
            Self::PublicationStarted => {
                output.map(|output| EventKind::PublicationStarted { output })
            }
            Self::Published => output.map(|output| EventKind::Published { output }),
            Self::Failed(code) => Some(EventKind::Failed { code }),
            Self::Cancelled => Some(EventKind::Cancelled),
        }
    }
}

/// Key/output are stored once.  Rejected untrusted input remains in `CompactReject` rather than
/// being erased or being admitted to the durable accepted log.
struct CompactWorkflowLog {
    key: Option<StageKey>,
    output: Option<ContentId<ObjectDomain>>,
    accepted: Vec<AcceptedTag>,
    state: WorkflowState,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CompactReject {
    event: WorkflowEvent,
    cause: ReductionError,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CompactReplayError {
    MissingOutput { tag: &'static str },
    Reduction(ReductionError),
}

impl CompactWorkflowLog {
    fn new(capacity: usize) -> Self {
        Self {
            key: None,
            output: None,
            accepted: Vec::with_capacity(capacity),
            state: WorkflowState::empty(),
        }
    }
    #[allow(
        clippy::result_large_err,
        reason = "the lab must retain the rejected untrusted event and exact reduction cause"
    )]
    fn append(&mut self, event: WorkflowEvent) -> Result<(), CompactReject> {
        let reduction =
            reduce(self.state, event).map_err(|cause| CompactReject { event, cause })?;
        if self.key.is_none() {
            self.key = Some(event.key);
        }
        match event.kind {
            EventKind::Staged { output }
            | EventKind::Verified { output }
            | EventKind::PublicationStarted { output }
            | EventKind::Published { output } => {
                if self.output.is_none() {
                    self.output = Some(output);
                }
            }
            EventKind::Requested
            | EventKind::Admitted
            | EventKind::Failed { .. }
            | EventKind::Cancelled => {}
        }
        self.accepted.push(AcceptedTag::from_event(event.kind));
        self.state = reduction.state;
        Ok(())
    }
    fn replay(&self) -> Result<WorkflowState, CompactReplayError> {
        let Some(key) = self.key else {
            return Ok(WorkflowState::empty());
        };
        let mut state = WorkflowState::empty();
        for tag in self.accepted.iter().copied() {
            let Some(kind) = tag.event(self.output) else {
                return Err(CompactReplayError::MissingOutput {
                    tag: "output-bearing accepted tag",
                });
            };
            state = reduce(
                state,
                WorkflowEvent {
                    version: WorkflowVersion::WAVE1,
                    key,
                    kind,
                },
            )
            .map_err(CompactReplayError::Reduction)?
            .state;
        }
        Ok(state)
    }
    fn retained_bytes(&self) -> usize {
        size_of::<Self>() + self.accepted.capacity() * size_of::<AcceptedTag>()
    }
}

fn workflow_key(seed: u8) -> StageKey {
    StageKey::derive(
        OperationId::PinnedObject,
        StageId::HydrateObject,
        &StageInput {
            generation: GenerationId::from_digest([seed; 32]),
            content: ContentId::<ObjectDomain>::from_digest([seed.wrapping_add(1); 32]),
        },
        ContentId::<CapabilityDomain>::from_digest([seed.wrapping_add(2); 32]),
        ContentId::<ConfigurationDomain>::from_digest([seed.wrapping_add(3); 32]),
    )
}
fn workflow_events(key: StageKey) -> [WorkflowEvent; 6] {
    let output = ContentId::<ObjectDomain>::from_digest([9; 32]);
    [
        WorkflowEvent {
            version: WorkflowVersion::WAVE1,
            key,
            kind: EventKind::Requested,
        },
        WorkflowEvent {
            version: WorkflowVersion::WAVE1,
            key,
            kind: EventKind::Admitted,
        },
        WorkflowEvent {
            version: WorkflowVersion::WAVE1,
            key,
            kind: EventKind::Staged { output },
        },
        WorkflowEvent {
            version: WorkflowVersion::WAVE1,
            key,
            kind: EventKind::Verified { output },
        },
        WorkflowEvent {
            version: WorkflowVersion::WAVE1,
            key,
            kind: EventKind::PublicationStarted { output },
        },
        WorkflowEvent {
            version: WorkflowVersion::WAVE1,
            key,
            kind: EventKind::Published { output },
        },
    ]
}

// -------------------------------------------------------------------------------------------------
// Unsafe permit-addressed ready payload candidate. Keep it in the lab: the safe baselines below
// are the production-shaped comparison and no lock-free claim is made by this module.

struct PermitSlot<Payload> {
    state: AtomicU64,
    payload: UnsafeCell<MaybeUninit<Payload>>,
}
// SAFETY: all payload access is exclusively owned by a bitmap permit plus a state transition. A
// `Payload: Send` value may cross the producer/consumer boundary, and no shared `&Payload` escapes.
unsafe impl<Payload: Send> Sync for PermitSlot<Payload> {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Permit {
    index: usize,
    epoch: u64,
}

struct PermitQueue<Payload> {
    free: AtomicU64,
    ready: AtomicU64,
    slots: Vec<PermitSlot<Payload>>,
}

impl<Payload> PermitQueue<Payload> {
    fn new(capacity: usize) -> Self {
        assert!(capacity <= 64, "the bitmap proof covers 0..=64 slots");
        let free = if capacity == 64 {
            u64::MAX
        } else {
            (1_u64 << capacity).wrapping_sub(1)
        };
        Self {
            free: AtomicU64::new(free),
            ready: AtomicU64::new(0),
            slots: (0..capacity)
                .map(|_| PermitSlot {
                    state: AtomicU64::new(pack(0, FREE)),
                    payload: UnsafeCell::new(MaybeUninit::uninit()),
                })
                .collect(),
        }
    }
    fn try_publish(&self, payload: Payload) -> Result<Permit, Payload> {
        loop {
            let Some(index) = claim_set_bit(&self.free) else {
                return Err(payload);
            };
            let slot = &self.slots[index];
            let observed = slot.state.load(Ordering::Acquire);
            if unpack_status(observed) == RETIRED {
                continue;
            }
            if unpack_status(observed) != FREE {
                self.free.fetch_or(1_u64 << index, Ordering::Release);
                return Err(payload);
            }
            let prior_epoch = unpack_epoch(observed);
            if prior_epoch == u64::MAX >> STATUS_BITS {
                let _retired = slot.state.compare_exchange(
                    observed,
                    pack(prior_epoch, RETIRED),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
                continue;
            }
            let epoch = prior_epoch + 1;
            slot.state.store(pack(epoch, ACTIVE), Ordering::Release);
            // SAFETY: claiming `free[index]` gives this producer exclusive access. No consumer may
            // read the slot until the following READY release publication, and the cell is aligned and
            // allocated as part of `PermitSlot` for its complete lifetime.
            unsafe { (*slot.payload.get()).write(payload) };
            slot.state.store(pack(epoch, READY), Ordering::Release);
            // This release is the ready-bitmap publication point; a consumer acquires it before read.
            self.ready.fetch_or(1_u64 << index, Ordering::Release);
            return Ok(Permit { index, epoch });
        }
    }
    fn try_take(&self) -> Option<Payload> {
        loop {
            let index = claim_set_bit(&self.ready)?;
            let slot = &self.slots[index];
            let observed = slot.state.load(Ordering::Acquire);
            if unpack_status(observed) != READY {
                continue;
            } // cancelled/reused stale ready bit
            if slot
                .state
                .compare_exchange(
                    observed,
                    pack(unpack_epoch(observed), CLAIMED),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_err()
            {
                continue;
            }
            // SAFETY: this consumer owns the only cleared ready permit and won READY->CLAIMED. The
            // producer initialized once-before READY release; no other read/drop may alias it.
            let payload = unsafe { (*slot.payload.get()).assume_init_read() };
            self.release(index, unpack_epoch(observed));
            return Some(payload);
        }
    }
    fn cancel(&self, permit: Permit) -> bool {
        let Some(slot) = self.slots.get(permit.index) else {
            return false;
        };
        if slot
            .state
            .compare_exchange(
                pack(permit.epoch, READY),
                pack(permit.epoch, CANCELLED),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return false;
        }
        // SAFETY: winning READY->CANCELLED grants this canceller exclusive initialized ownership;
        // a simultaneous taker/canceller loses its CAS. Drop happens before slot reuse/free.
        unsafe { (*slot.payload.get()).assume_init_drop() };
        self.release(permit.index, permit.epoch);
        true
    }
    fn retire_free(&self, index: usize) -> bool {
        let Some(slot) = self.slots.get(index) else {
            return false;
        };
        let bit = 1_u64 << index;
        let mut free = self.free.load(Ordering::Acquire);
        loop {
            if free & bit == 0 {
                return false;
            }
            match self.free.compare_exchange_weak(
                free,
                free & !bit,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(actual) => free = actual,
            }
        }
        // The cleared free bit is the linear permit: a producer cannot begin writing this slot
        // while retirement checks and changes its state.
        let observed = slot.state.load(Ordering::Acquire);
        if unpack_status(observed) != FREE {
            self.free.fetch_or(bit, Ordering::Release);
            return false;
        }
        match slot.state.compare_exchange(
            observed,
            pack(unpack_epoch(observed), RETIRED),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => true,
            Err(_) => {
                self.free.fetch_or(bit, Ordering::Release);
                false
            }
        }
    }
    fn release(&self, index: usize, epoch: u64) {
        self.slots[index]
            .state
            .store(pack(epoch, FREE), Ordering::Release);
        self.free.fetch_or(1_u64 << index, Ordering::Release);
    }
}
impl<Payload> Drop for PermitQueue<Payload> {
    fn drop(&mut self) {
        // `&mut self` proves callers have ceased using the queue. READY is the only initialized
        // non-claimed state the public methods can leave; claimed values are moved/dropped before
        // their permit is released. This also covers cancellation/reuse and partial initialization.
        for slot in &mut self.slots {
            if unpack_status(slot.state.load(Ordering::Relaxed)) == READY {
                // SAFETY: READY was initialized exactly once and has not been claimed. `&mut self`
                // excludes concurrent producer/consumer aliases during destruction.
                unsafe { (*slot.payload.get()).assume_init_drop() };
            }
        }
    }
}

fn pack(epoch: u64, status: u64) -> u64 {
    (epoch << STATUS_BITS) | status
}
fn unpack_epoch(state: u64) -> u64 {
    state >> STATUS_BITS
}
fn unpack_status(state: u64) -> u64 {
    state & STATUS_MASK
}
fn claim_set_bit(bitmap: &AtomicU64) -> Option<usize> {
    let mut observed = bitmap.load(Ordering::Acquire);
    loop {
        if observed == 0 {
            return None;
        }
        let index = observed.trailing_zeros() as usize;
        let next = observed & !(1_u64 << index);
        match bitmap.compare_exchange_weak(observed, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return Some(index),
            Err(actual) => observed = actual,
        }
    }
}

struct MutexQueue<Payload>(Mutex<VecDeque<Payload>>);
impl<Payload> MutexQueue<Payload> {
    fn new() -> Self {
        Self(Mutex::new(VecDeque::new()))
    }
    fn publish(&self, payload: Payload) -> Result<(), Payload> {
        match self.0.lock() {
            Ok(mut queue) => {
                queue.push_back(payload);
                Ok(())
            }
            Err(_) => Err(payload),
        }
    }
    fn take(&self) -> Option<Payload> {
        match self.0.lock() {
            Ok(mut queue) => queue.pop_front(),
            Err(_) => None,
        }
    }
}

#[derive(Clone, Copy)]
struct Payload<const BYTES: usize>([u8; BYTES]);
impl<const BYTES: usize> Default for Payload<BYTES> {
    fn default() -> Self {
        Self([0; BYTES])
    }
}

fn permit_payload_work<const BYTES: usize>() -> usize {
    let permit = PermitQueue::<Payload<BYTES>>::new(8);
    let mut taken = 0;
    for value in 0..8_u8 {
        if permit.try_publish(Payload([value; BYTES])).is_ok() {
            taken += usize::from(permit.try_take().is_some());
        }
    }
    taken
}

fn mutex_payload_work<const BYTES: usize>() -> usize {
    let queue = MutexQueue::<Payload<BYTES>>::new();
    let mut taken = 0;
    for value in 0..8_u8 {
        if queue.publish(Payload([value; BYTES])).is_ok() {
            taken += usize::from(queue.take().is_some());
        }
    }
    taken
}

fn array_payload_work<const BYTES: usize>() -> usize {
    let queue = ArrayQueue::<Payload<BYTES>>::new(9);
    let mut taken = 0;
    for value in 0..8_u8 {
        if queue.push(Payload([value; BYTES])).is_ok() {
            taken += usize::from(queue.pop().is_some());
        }
    }
    taken
}

fn thing_payload_work<const BYTES: usize>() -> usize {
    let queue = thingbuf::ThingBuf::<Payload<BYTES>>::new(8);
    let mut taken = 0;
    for value in 0..8_u8 {
        if queue.push(Payload([value; BYTES])).is_ok() {
            taken += usize::from(queue.pop().is_some());
        }
    }
    taken
}

fn static_thing_payload_work<const BYTES: usize>() -> usize {
    let queue = thingbuf::StaticThingBuf::<Payload<BYTES>, 8>::new();
    let mut taken = 0;
    for value in 0..8_u8 {
        if queue.push(Payload([value; BYTES])).is_ok() {
            taken += usize::from(queue.pop().is_some());
        }
    }
    taken
}

/// Emits all measured rows. The output is intentionally normalized TSV, not a benchmark verdict.
pub fn run(allocator: &TrackingAllocator) {
    println!("format\tnudox-layout-experiments-v2");
    println!("target_arch\t{}", std::env::consts::ARCH);
    println!("target_os\t{}", std::env::consts::OS);
    println!("pointer_width\t{}", usize::BITS);
    println!("cache_line_assumption_bytes\t{CACHE_LINE_BYTES}");
    println!("counter\tlogical-row/cache-line estimates plus scoped requested-allocation bytes");
    hydration_rows(allocator);
    waiter_rows(allocator);
    locality::rows(allocator);
    recorder_rows();
    workflow_rows(allocator);
    queue_rows(allocator);
    store_rows(allocator);
}

/// Monomorphization controls for the locality representation comparison.
///
/// These functions remain in the nested lab only; their four tiny binaries are
/// compared by `run-locality-code-size.sh`, not linked into any client crate.
pub fn locality_text_current() -> usize {
    locality::text_current()
}

pub fn locality_text_fused() -> usize {
    locality::text_fused()
}

pub fn locality_text_split() -> usize {
    locality::text_split()
}

pub fn locality_text_packed() -> usize {
    locality::text_packed()
}

/// Monomorphization control for the deliberately unaligned 13-byte route baseline.
#[must_use]
pub fn locality_text_packed13() -> usize {
    locality::text_packed13()
}

#[allow(
    clippy::too_many_arguments,
    reason = "TSV columns intentionally mirror independent evidence fields"
)]
fn print_hydration(
    representation: &str,
    rows: usize,
    density: usize,
    actual_absent_rows: usize,
    retained: usize,
    same: bool,
    access: AccessFacts,
    allocation: AllocationFacts,
) {
    println!(
        "hydration\t{representation}\t{rows}\t{density}\t{actual_absent_rows}\t{retained}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        usize::from(same),
        access.visited_rows,
        access.branches,
        access.cache_lines,
        allocation.allocations,
        allocation.allocated_bytes,
        allocation.peak_live_bytes,
    );
}

fn hydration_rows(allocator: &TrackingAllocator) {
    println!(
        "columns\thydration\trepresentation\trows\trequested_absent_percent\tactual_absent_rows\tretained_bytes\texact_output\tvisited_rows\tbranches\tlogical_cache_lines\tallocations\tallocated_bytes\tpeak_live_bytes"
    );
    for rows in [0_usize, 1, 4, 5, 100_000] {
        for density in [0_usize, 1, 50, 100] {
            let routes = match coverage_input(rows, density) {
                Ok(routes) => routes,
                Err(error) => {
                    println!("hydration_setup_error\trows\t{rows}\tdensity\t{density}\t{error}");
                    continue;
                }
            };
            let actual_absent_rows = rows.saturating_mul(density) / 100;
            let expected = dense_build(&routes, density).enumerate().0;
            let ((retained, same, access), allocation) = allocator.measure(|| {
                let candidate = dense_build(&routes, density);
                let retained = candidate.retained_bytes();
                let (output, access) = candidate.enumerate();
                (retained, output == expected, access)
            });
            print_hydration(
                "dense_16B",
                rows,
                density,
                actual_absent_rows,
                retained,
                same,
                access,
                allocation,
            );
            let ((retained, same, access), allocation) = allocator.measure(|| {
                let candidate = sparse_build(&routes, density);
                let retained = candidate.retained_bytes();
                let (output, access) = candidate.enumerate(&routes);
                (retained, output == expected, access)
            });
            print_hydration(
                "sparse_u32_borrowed_route",
                rows,
                density,
                actual_absent_rows,
                retained,
                same,
                access,
                allocation,
            );
            let ((retained, same, access), allocation) = allocator.measure(|| {
                let candidate = bitset_build(&routes, density);
                let retained = candidate.retained_bytes();
                let (output, access) = candidate.enumerate(&routes);
                (retained, output == expected, access)
            });
            print_hydration(
                "dense_absent_bitset_borrowed_route",
                rows,
                density,
                actual_absent_rows,
                retained,
                same,
                access,
                allocation,
            );
        }
    }
}

fn print_waiter(
    representation: &str,
    capacity: usize,
    active: usize,
    access: AccessFacts,
    allocation: AllocationFacts,
) {
    println!(
        "waiter\t{representation}\t{capacity}\t{active}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        size_of::<DenseWaiter>(),
        access.visited_rows,
        access.branches,
        access.atomics,
        access.cache_lines,
        allocation.allocations,
        allocation.peak_live_bytes,
    );
}

fn waiter_rows(allocator: &TrackingAllocator) {
    println!(
        "columns\twaiter\trepresentation\tcapacity\tactive_random\tdense_record_bytes\trecord_visits\tbranches\tatomics\tlogical_cache_lines\tallocations\tpeak_live_bytes"
    );
    let _warm_waker = noop_waker();
    for capacity in [1_usize, 8, 64] {
        for active in [0_usize, 1, 8, 64] {
            if active > capacity {
                continue;
            }
            let positions = active_positions(active, capacity);
            let (facts, allocation) = allocator.measure(|| {
                let waiters = DenseWaiters::new(capacity, &positions);
                waiters.wake(1)
            });
            print_waiter(
                "aos_dense_atomic_waker",
                capacity,
                active,
                facts,
                allocation,
            );
            let (facts, allocation) = allocator.measure(|| {
                let waiters = SoaWaiters::new(capacity, &positions);
                waiters.wake(1)
            });
            print_waiter(
                "soa_state_credit_waker",
                capacity,
                active,
                facts,
                allocation,
            );
            let (facts, allocation) = allocator.measure(|| {
                let waiters = BitmapWaiters::new(capacity, &positions);
                waiters.wake(1)
            });
            print_waiter(
                "active_u64_bitmap_plus_dense",
                capacity,
                active,
                facts,
                allocation,
            );
            let (facts, allocation) = allocator.measure(|| {
                let waiters = BucketWaiters::new(capacity, &positions);
                waiters.wake(1)
            });
            print_waiter(
                "byte_fit_bucket_epoch_ticket",
                capacity,
                active,
                facts,
                allocation,
            );
        }
    }
    let bitmap = BitmapWaiters::new(1, &[0]);
    let snapshot = bitmap.active.load(Ordering::Acquire);
    bitmap.cancel(0);
    let cancelled = bitmap.wake_snapshot(snapshot, 1).visited_rows;
    bitmap.rearm(0);
    let rearmed = bitmap.wake(1).visited_rows;
    println!(
        "waiter_schedule\tbitmap_cancel_stale_snapshot_rearm\tstale_bits_visited\t{cancelled}\trearmed_bits_visited\t{rearmed}\tsemantic\tstale_cancelled_bit_never_wakes;_rearm_may_receive_benign_recheck_wake"
    );
}

fn recorder_layout_rows<Event>(name: &str)
where
    Event: Copy,
{
    for capacity in [0_usize, 1, 4, 64] {
        match capacity {
            0 => print_recorder_pair::<Event, 0>(name),
            1 => print_recorder_pair::<Event, 1>(name),
            4 => print_recorder_pair::<Event, 4>(name),
            64 => print_recorder_pair::<Event, 64>(name),
            _ => {}
        }
    }
}
fn print_recorder_pair<Event: Copy, const CAP: usize>(event: &str) {
    println!(
        "recorder\toption_array_head_len\t{event}\t{CAP}\t{}\t{}",
        size_of::<OptionRing<Event, CAP>>(),
        size_of::<Option<Event>>()
    );
    println!(
        "recorder\tarrayvec_head\t{event}\t{CAP}\t{}\t{}",
        size_of::<ArrayVecRing<Event, CAP>>(),
        size_of::<Event>()
    );
}
fn recorder_rows() {
    println!("columns\trecorder\trepresentation\tevent\tcapacity\tstack_bytes\tper_slot_bytes");
    recorder_layout_rows::<Event1>("1");
    recorder_layout_rows::<Event8>("8");
    recorder_layout_rows::<Event68>("68");
    recorder_layout_rows::<Event128>("128");
    let mut option = OptionRing::<u8, 4>::new();
    let mut array = ArrayVecRing::<u8, 4>::new();
    for event in 0..6 {
        option.record(event);
        array.record(event);
    }
    println!(
        "recorder_semantics\toverwrite_order\texact_output\t{}",
        usize::from(option.events().eq(array.events()))
    );
}

fn workflow_rows(allocator: &TrackingAllocator) {
    println!(
        "columns\tworkflow\trepresentation\tprefix\tretained_bytes\treplay_events\texact_state\tallocations\tpeak_live_bytes"
    );
    let key = workflow_key(1);
    let events = workflow_events(key);
    for prefix in 1..=events.len() {
        let ((retained, replay_events, same), allocation) = allocator.measure(|| {
            let mut ordinary = match MemoryWorkflowLog::new(events.len()) {
                Ok(log) => log,
                Err(_) => return (0, 0, false),
            };
            for event in events[..prefix].iter().copied() {
                if ordinary.append_then_reduce(event).is_err() {
                    return (0, 0, false);
                }
            }
            let same = ordinary.replay().is_ok();
            (
                size_of::<MemoryWorkflowLog>() + events.len() * size_of::<WorkflowEvent>(),
                prefix,
                same,
            )
        });
        println!(
            "workflow\tvec_workflow_event\t{prefix}\t{retained}\t{replay_events}\t{}\t{}\t{}",
            usize::from(same),
            allocation.allocations,
            allocation.peak_live_bytes
        );
        let ((retained, replay_events, same), allocation) = allocator.measure(|| {
            let mut compact = CompactWorkflowLog::new(events.len());
            for event in events[..prefix].iter().copied() {
                if compact.append(event).is_err() {
                    return (0, 0, false);
                }
            }
            let same = match (compact.replay(), replay_state(&events[..prefix])) {
                (Ok(compact), Ok(reference)) => compact == reference,
                _ => false,
            };
            (compact.retained_bytes(), prefix, same)
        });
        println!(
            "workflow\tper_key_compact_accepted\t{prefix}\t{retained}\t{replay_events}\t{}\t{}\t{}",
            usize::from(same),
            allocation.allocations,
            allocation.peak_live_bytes
        );
    }
    let first = workflow_events(workflow_key(1));
    let second = workflow_events(workflow_key(2));
    let mut order = Vec::with_capacity(6);
    for index in 0..3_u8 {
        order.push((0_u8, index));
        order.push((1_u8, index));
    }
    let interleaved_event_bytes =
        size_of::<MemoryWorkflowLog>() + order.capacity() * size_of::<WorkflowEvent>();
    let compact_order_bytes = size_of::<CompactWorkflowLog>() * 2
        + 3 * size_of::<AcceptedTag>() * 2
        + order.capacity() * size_of::<(u8, u8)>();
    println!(
        "workflow_interleave\tglobal_vec\t{}\t{}",
        order.len(),
        interleaved_event_bytes
    );
    println!(
        "workflow_interleave\tper_key_plus_global_order\t{}\t{}",
        order.len(),
        compact_order_bytes
    );
    let _keys = (first, second);
}
fn replay_state(events: &[WorkflowEvent]) -> Result<WorkflowState, ReductionError> {
    let mut state = WorkflowState::empty();
    for event in events {
        state = reduce(state, *event)?.state;
    }
    Ok(state)
}

fn queue_rows(allocator: &TrackingAllocator) {
    println!(
        "columns\tready_queue\trepresentation\tpayload_bytes\ttaken\tallocations\tallocated_bytes\tpeak_live_bytes\tstack_or_header_bytes"
    );
    macro_rules! row {
        ($bytes:literal) => {{
            macro_rules! measure {
                ($name:literal, $work:ident, $type:ty) => {{
                    let (taken, allocation) = allocator.measure($work::<$bytes>);
                    println!(
                        "ready_queue\t{}\t{}\t{taken}\t{}\t{}\t{}\t{}",
                        $name,
                        $bytes,
                        allocation.allocations,
                        allocation.allocated_bytes,
                        allocation.peak_live_bytes,
                        size_of::<$type>()
                    );
                }};
            }
            measure!(
                "permit_slots_unsafe_lab",
                permit_payload_work,
                PermitQueue<Payload<$bytes>>
            );
            measure!(
                "mutex_vecdeque_safe",
                mutex_payload_work,
                MutexQueue<Payload<$bytes>>
            );
            measure!(
                "arrayqueue_capacity_plus_one",
                array_payload_work,
                ArrayQueue<Payload<$bytes>>
            );
            measure!(
                "thingbuf_dynamic_safe",
                thing_payload_work,
                thingbuf::ThingBuf<Payload<$bytes>>
            );
            measure!(
                "thingbuf_static_safe",
                static_thing_payload_work,
                thingbuf::StaticThingBuf<Payload<$bytes>, 8>
            );
        }};
    }
    row!(0);
    row!(8);
    row!(64);
    row!(4096);
    let permits = PermitQueue::<u8>::new(1);
    let cancelled = match permits.try_publish(7) {
        Ok(ticket) => permits.cancel(ticket),
        Err(_) => false,
    };
    let retired = permits.retire_free(0);
    println!(
        "ready_queue_schedule\tpublish_cancel_retire\tcancelled\t{}\tretired\t{}\tstatus_cancelled_code\t{CANCELLED}",
        usize::from(cancelled),
        usize::from(retired)
    );
}

fn store_rows(allocator: &TrackingAllocator) {
    println!(
        "columns\tstore\tinline_entries\tinline_buckets\tslots\tallocation_count\tallocated_bytes\tpeak_live_bytes\tdrop_deallocations"
    );
    for slots in [0_u32, 1, 4, 5, 100_000] {
        let (_, allocation) = allocator.measure(|| {
            let store = MemoryStore::<ObjectDomain>::new(StoreCapacity {
                bytes: u64::from(slots).saturating_mul(8).into(),
                slots: slots.into(),
            });
            drop(store);
        });
        println!(
            "store\t4\t8\t{slots}\t{}\t{}\t{}\t{}",
            allocation.allocations,
            allocation.allocated_bytes,
            allocation.peak_live_bytes,
            allocation.deallocations
        );
    }
    for (entries, buckets) in [(0_usize, 0_usize), (1, 2), (4, 8), (64, 128)] {
        println!(
            "store_layout\tinline_entries\t{entries}\tinline_buckets\t{buckets}\tpublic_type_bytes\t{}",
            match (entries, buckets) {
                (0, 0) => size_of::<InlineMemoryStore<ObjectDomain, 0, 0>>(),
                (1, 2) => size_of::<InlineMemoryStore<ObjectDomain, 1, 2>>(),
                (4, 8) => size_of::<InlineMemoryStore<ObjectDomain, 4, 8>>(),
                (64, 128) => size_of::<InlineMemoryStore<ObjectDomain, 64, 128>>(),
                _ => 0,
            }
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn hydration_models_preserve_exact_coverage_output_at_all_cliffs()
    -> Result<(), nudox_object::ProviderIdError> {
        for rows in [0_usize, 1, 4, 5, 100_000] {
            for density in [0_usize, 1, 50, 100] {
                let routes = coverage_input(rows, density)?;
                let expected = dense_build(&routes, density).enumerate().0;
                let sparse = sparse_build(&routes, density).enumerate(&routes).0;
                let bitset = bitset_build(&routes, density).enumerate(&routes).0;
                assert_eq!(sparse, expected, "sparse rows={rows} density={density}");
                assert_eq!(bitset, expected, "bitset rows={rows} density={density}");
            }
        }
        Ok(())
    }

    #[test]
    fn bitmap_stale_snapshot_skips_cancelled_slot_and_rearm_is_explicit() {
        let waiters = BitmapWaiters::new(1, &[0]);
        let snapshot = waiters.active.load(Ordering::Acquire);
        waiters.cancel(0);
        let stale = waiters.wake_snapshot(snapshot, 1);
        assert_eq!(stale.visited_rows, 1);
        assert_eq!(waiters.slots[0].state.load(Ordering::Acquire), FREE);
        waiters.rearm(0);
        assert_eq!(waiters.wake(1).visited_rows, 1);
        assert_eq!(waiters.slots[0].state.load(Ordering::Acquire), ACTIVE);
    }

    #[test]
    fn recorder_models_keep_tail_order_across_wrap() {
        let mut option = OptionRing::<u8, 4>::new();
        let mut array = ArrayVecRing::<u8, 4>::new();
        for event in 0..6 {
            option.record(event);
            array.record(event);
        }
        assert!(option.events().eq(array.events()));
        assert_eq!(
            option.events().copied().collect::<Vec<_>>(),
            vec![2, 3, 4, 5]
        );
    }

    #[test]
    fn recorder_models_drop_replaced_event_once() {
        #[derive(Clone)]
        struct DropSpy {
            id: u8,
            dropped: Arc<Mutex<Vec<u8>>>,
        }
        impl Drop for DropSpy {
            fn drop(&mut self) {
                if let Ok(mut values) = self.dropped.lock() {
                    values.push(self.id);
                }
            }
        }
        let drops = Arc::new(Mutex::new(Vec::new()));
        {
            let mut option = OptionRing::<DropSpy, 1>::new();
            option.record(DropSpy {
                id: 1,
                dropped: Arc::clone(&drops),
            });
            option.record(DropSpy {
                id: 2,
                dropped: Arc::clone(&drops),
            });
        }
        let observed = match drops.lock() {
            Ok(values) => values.clone(),
            Err(_) => Vec::new(),
        };
        assert_eq!(observed, vec![1, 2]);
    }

    #[test]
    fn compact_log_replays_every_durable_prefix_and_preserves_rejected_event() {
        let key = workflow_key(1);
        let events = workflow_events(key);
        for prefix in 1..=events.len() {
            let mut compact = CompactWorkflowLog::new(events.len());
            for event in events[..prefix].iter().copied() {
                assert_eq!(compact.append(event), Ok(()));
            }
            let observed = compact.replay().ok();
            let expected = replay_state(&events[..prefix]).ok();
            assert_eq!(observed, expected);
        }
        let mut compact = CompactWorkflowLog::new(6);
        let requested = events[0];
        assert_eq!(compact.append(requested), Ok(()));
        let rejected = WorkflowEvent {
            key: workflow_key(2),
            ..requested
        };
        let evidence = match compact.append(rejected) {
            Err(CompactReject {
                event,
                cause: ReductionError::StageKeyMismatch { observed, .. },
            }) => Some((event, observed)),
            Ok(()) | Err(_) => None,
        };
        assert_eq!(evidence, Some((rejected, rejected.key)));
    }

    #[test]
    fn permit_queue_matches_safe_fifo_for_reuse_and_cancellation() {
        let queue = PermitQueue::<u8>::new(4);
        let safe = MutexQueue::<u8>::new();
        let mut tickets = Vec::new();
        for value in 0..4_u8 {
            let result = queue.try_publish(value);
            assert!(result.is_ok());
            if let Ok(ticket) = result {
                tickets.push(ticket);
            }
            assert_eq!(safe.publish(value), Ok(()));
        }
        let cancelled = tickets[1];
        assert!(queue.cancel(cancelled));
        assert_eq!(safe.take(), Some(0));
        assert_eq!(queue.try_take(), Some(0));
        assert_eq!(safe.take(), Some(1)); // cancellation owns the matching reference payload.
        assert_eq!(safe.take(), Some(2));
        assert_eq!(queue.try_take(), Some(2));
        assert_eq!(safe.take(), Some(3));
        assert_eq!(queue.try_take(), Some(3));
        for value in 4..64_u8 {
            assert!(queue.try_publish(value).is_ok());
            assert_eq!(safe.publish(value), Ok(()));
            assert_eq!(queue.try_take(), safe.take());
        }
        assert!(queue.retire_free(0));
        assert!(queue.try_publish(99).is_ok());
    }

    #[test]
    fn permit_queue_cancellation_and_owner_drop_destroy_each_payload_once() {
        struct Counted(Arc<AtomicUsize>);
        impl Drop for Counted {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let drops = Arc::new(AtomicUsize::new(0));
        {
            let queue = PermitQueue::new(2);
            let first = queue.try_publish(Counted(Arc::clone(&drops))).ok();
            let _second = queue.try_publish(Counted(Arc::clone(&drops)));
            if let Some(ticket) = first {
                assert!(queue.cancel(ticket));
            }
        }
        assert_eq!(drops.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn loom_ready_bitmap_publication_model_has_no_lost_ready_bit() {
        loom::model(|| {
            let ready = Arc::new(loom::sync::atomic::AtomicU64::new(0));
            let published = Arc::new(loom::sync::atomic::AtomicU64::new(0));
            let producer_ready = Arc::clone(&ready);
            let producer_published = Arc::clone(&published);
            let producer = loom::thread::spawn(move || {
                producer_published.store(1, loom::sync::atomic::Ordering::Release);
                producer_ready.fetch_or(1, loom::sync::atomic::Ordering::Release);
            });
            let consumer_ready = Arc::clone(&ready);
            let consumer_published = Arc::clone(&published);
            let consumer = loom::thread::spawn(move || {
                let bitmap = consumer_ready.load(loom::sync::atomic::Ordering::Acquire);
                if bitmap & 1 != 0 {
                    assert_eq!(
                        consumer_published.load(loom::sync::atomic::Ordering::Acquire),
                        1
                    );
                }
            });
            assert!(producer.join().is_ok());
            assert!(consumer.join().is_ok());
        });
    }
}
