//! Defines runtime behavior for `server-runtime`, whose purpose is to schedule bounded work with explicit credits, ownership, and wakeups.
//! This module owns the runtime invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Fabric construction, scoped producer/owner splitting, and quiescent metrics.
#![allow(
    missing_docs,
    reason = "construction failures retain their exact allocation or configured-bound cause"
)]
#![allow(
    clippy::missing_const_for_fn,
    clippy::missing_errors_doc,
    reason = "the scoped split exposes ordinary references and construction errors are its public contract"
)]
#![allow(
    type_alias_bounds,
    reason = "RuntimeSplit retains its storage-policy bound in generated documentation until Rust enforces alias bounds"
)]

use alloc::collections::TryReserveError;
use core::{
    num::NonZeroU8,
    sync::atomic::{AtomicU64, Ordering},
};

use thiserror::Error;

use crate::{
    BoundedWork, ByteBudget, RuntimeHistory, RuntimeMetrics,
    budget::CreditPool,
    metrics::{Accounting, ObservedAccounting},
    payload_slot::PayloadSlot,
    ready_bitmap::ReadyBitmap,
    slot::{RuntimeIdentity, SlotIndex},
    storage::{
        InlinePayloads, InlineStorage, LocalRuntimeArena, LocalStorage, RemotePayloads,
        RemoteStorage, RuntimePayloadTable, RuntimeStorage,
    },
    waiter::{InlineWaiterTable, WaiterRegistry},
    work_permit::{MAX_WORK_SLOTS, WorkPermits},
};

static NEXT_RUNTIME_IDENTITY: AtomicU64 = AtomicU64::new(1);

/// The one validated source of truth for every fixed work/waiter coordinate domain.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RuntimeGeometry {
    work_slots: NonZeroU8,
    waiter_slots: NonZeroU8,
}

impl RuntimeGeometry {
    pub(crate) fn new(work_slots: usize, waiter_slots: usize) -> Result<Self, RuntimeConfigError> {
        Ok(Self {
            work_slots: validate_slot_capacity(work_slots, GeometryAxis::Work)?,
            waiter_slots: validate_slot_capacity(waiter_slots, GeometryAxis::Waiter)?,
        })
    }

    fn work_capacity(self) -> usize {
        usize::from(self.work_slots.get())
    }

    fn waiter_capacity(self) -> usize {
        usize::from(self.waiter_slots.get())
    }
}

/// Shared bounded state. Producers mutate only permits, payload coordinates, and ready bits.
pub(crate) struct Fabric<
    Generation,
    Work,
    Failure,
    AccountingPolicy,
    StoragePolicy: RuntimeStorage<Generation, Work, Failure>,
> {
    pub(crate) identity: RuntimeIdentity,
    pub(crate) work_permits: WorkPermits,
    pub(crate) byte_credits: CreditPool,
    pub(crate) ready: ReadyBitmap,
    pub(crate) terminal_ready: ReadyBitmap,
    pub(crate) payloads: StoragePolicy::Payloads,
    pub(crate) budget: ByteBudget,
    geometry: RuntimeGeometry,
    pub(crate) accounting: AccountingPolicy,
    pub(crate) waiters: WaiterRegistry<StoragePolicy::Waiters>,
}

/// Runtime owns the fabric and the only terminal-retention queue.
pub struct Runtime<
    Generation,
    Work,
    Failure,
    AccountingPolicy,
    StoragePolicy: RuntimeStorage<Generation, Work, Failure>,
> {
    pub(crate) fabric: Fabric<Generation, Work, Failure, AccountingPolicy, StoragePolicy>,
}

/// Explicit heap-reserved runtime for a dynamic/remote execution boundary.
pub type RemoteRuntime<
    Generation,
    Work,
    Failure = core::convert::Infallible,
    AccountingPolicy = (),
> = Runtime<Generation, Work, Failure, AccountingPolicy, RemoteStorage>;

/// Const-inline runtime with no retained heap table or terminal queue allocation.
pub type InlineRuntime<
    Generation,
    Work,
    const WORK_SLOTS: usize,
    const WAITER_SLOTS: usize,
    Failure = core::convert::Infallible,
    AccountingPolicy = (),
> = Runtime<Generation, Work, Failure, AccountingPolicy, InlineStorage<WORK_SLOTS, WAITER_SLOTS>>;

/// Caller-arena runtime whose storage lifetime is visible in its type.
pub type LocalRuntime<
    'arena,
    Generation,
    Work,
    Failure = core::convert::Infallible,
    AccountingPolicy = (),
> = Runtime<Generation, Work, Failure, AccountingPolicy, LocalStorage<'arena>>;

/// Scoped producer/owner pair borrowing one runtime fabric.
pub type RuntimeSplit<
    'runtime,
    Generation,
    Work,
    Failure,
    AccountingPolicy,
    StoragePolicy: RuntimeStorage<Generation, Work, Failure>,
> = (
    crate::Admission<'runtime, Generation, Work, AccountingPolicy, StoragePolicy, Failure>,
    crate::Owner<'runtime, Generation, Work, Failure, AccountingPolicy, StoragePolicy>,
);

/// Construction failure before accepting any work.
#[derive(Debug, Error)]
pub enum RuntimeConfigError {
    #[error("a runtime needs at least one work slot")]
    ZeroWorkSlots,
    #[error("work capacity {requested} exceeds the {maximum}-slot atomic permit bitmap")]
    WorkSlotsExceedBitmap { requested: usize, maximum: usize },
    #[error("a runtime needs at least one async waiter slot")]
    ZeroAsyncWaiters,
    #[error("async waiter capacity {requested} exceeds the {maximum}-slot bounded registry")]
    AsyncWaitersExceedBound { requested: usize, maximum: usize },
    #[error("reserving the fixed slot table failed")]
    SlotTableAllocation {
        #[source]
        source: TryReserveError,
    },
    #[error("reserving the fixed async waiter table failed")]
    WaiterTableAllocation {
        #[source]
        source: TryReserveError,
    },
    #[error("the process-local runtime identity sequence is exhausted")]
    RuntimeIdentityExhausted,
}

impl<Generation, Work: BoundedWork, Failure, AccountingPolicy: Accounting>
    Runtime<Generation, Work, Failure, AccountingPolicy, RemoteStorage>
{
    /// Allocates every retained queue/table before the first producer can acquire a permit.
    pub fn new(work_capacity: usize, budget: ByteBudget) -> Result<Self, RuntimeConfigError> {
        Self::with_waiter_capacity(work_capacity, budget, work_capacity)
    }

    /// Allocates independent bounded async registrations in addition to physical work slots.
    pub fn with_waiter_capacity(
        work_capacity: usize,
        budget: ByteBudget,
        waiter_capacity: usize,
    ) -> Result<Self, RuntimeConfigError> {
        let geometry = RuntimeGeometry::new(work_capacity, waiter_capacity)?;
        let identity = allocate_runtime_identity()?;
        let payloads = RemotePayloads::try_new(geometry.work_capacity())
            .map_err(|source| RuntimeConfigError::SlotTableAllocation { source })?;
        let waiters = WaiterRegistry::new(geometry.waiter_capacity())
            .map_err(|source| RuntimeConfigError::WaiterTableAllocation { source })?;
        Ok(Self {
            fabric: Fabric {
                identity,
                work_permits: WorkPermits::new(geometry.work_capacity()),
                byte_credits: CreditPool::new(budget.credits),
                ready: ReadyBitmap::new(),
                terminal_ready: ReadyBitmap::new(),
                payloads,
                budget,
                geometry,
                accounting: AccountingPolicy::default(),
                waiters,
            },
        })
    }
}

impl<
    Generation,
    Work: BoundedWork,
    Failure,
    AccountingPolicy: Accounting,
    const WORK_SLOTS: usize,
    const WAITER_SLOTS: usize,
> Runtime<Generation, Work, Failure, AccountingPolicy, InlineStorage<WORK_SLOTS, WAITER_SLOTS>>
{
    /// Constructs an inline runtime without retaining a heap table or queue allocation.
    pub fn new(budget: ByteBudget) -> Result<Self, RuntimeConfigError> {
        let geometry = RuntimeGeometry::new(WORK_SLOTS, WAITER_SLOTS)?;
        let identity = allocate_runtime_identity()?;
        Ok(Self {
            fabric: Fabric {
                identity,
                work_permits: WorkPermits::new(geometry.work_capacity()),
                byte_credits: CreditPool::new(budget.credits),
                ready: ReadyBitmap::new(),
                terminal_ready: ReadyBitmap::new(),
                payloads: InlinePayloads::new(),
                budget,
                geometry,
                accounting: AccountingPolicy::default(),
                waiters: WaiterRegistry::from_table(InlineWaiterTable::new()),
            },
        })
    }
}

impl<
    'arena,
    Generation: 'arena,
    Work: BoundedWork + 'arena,
    Failure: 'arena,
    AccountingPolicy: Accounting,
> Runtime<Generation, Work, Failure, AccountingPolicy, LocalStorage<'arena>>
{
    /// Borrows caller-provided fixed storage without a runtime allocation or ownership wrapper.
    pub fn from_arena(
        budget: ByteBudget,
        arena: LocalRuntimeArena<'arena, Generation, Work, Failure>,
    ) -> Result<Self, RuntimeConfigError> {
        let geometry = arena.geometry()?;
        let identity = allocate_runtime_identity()?;
        let crate::storage::LocalStorageTables { payloads, waiters } = arena.initialize();
        Ok(Self {
            fabric: Fabric {
                identity,
                work_permits: WorkPermits::new(geometry.work_capacity()),
                byte_credits: CreditPool::new(budget.credits),
                ready: ReadyBitmap::new(),
                terminal_ready: ReadyBitmap::new(),
                payloads,
                budget,
                geometry,
                accounting: AccountingPolicy::default(),
                waiters: WaiterRegistry::from_table(waiters),
            },
        })
    }
}

impl<
    Generation,
    Work,
    Failure,
    AccountingPolicy,
    StoragePolicy: RuntimeStorage<Generation, Work, Failure>,
> Runtime<Generation, Work, Failure, AccountingPolicy, StoragePolicy>
where
    StoragePolicy::Payloads: RuntimePayloadTable<Generation, Work, Failure>,
{
    /// Splits the scoped MPMC admission surface from its one owner drain.
    pub fn split(
        &mut self,
    ) -> RuntimeSplit<'_, Generation, Work, Failure, AccountingPolicy, StoragePolicy> {
        let fabric = &self.fabric;
        (crate::Admission { fabric }, crate::Owner { fabric })
    }

    /// Returns eventually consistent telemetry; conservation is exact after producers quiesce.
    #[must_use]
    pub fn metrics(&self) -> RuntimeMetrics {
        metrics(&self.fabric)
    }
}

impl<
    Generation,
    Work,
    Failure,
    AccountingPolicy: ObservedAccounting,
    StoragePolicy: RuntimeStorage<Generation, Work, Failure>,
> Runtime<Generation, Work, Failure, AccountingPolicy, StoragePolicy>
{
    /// Returns the optional policy's eventually consistent history.
    #[must_use]
    pub fn history(&self) -> RuntimeHistory {
        self.fabric.accounting.history()
    }
}

impl<
    Generation,
    Work,
    Failure,
    AccountingPolicy,
    StoragePolicy: RuntimeStorage<Generation, Work, Failure>,
> Fabric<Generation, Work, Failure, AccountingPolicy, StoragePolicy>
where
    StoragePolicy::Payloads: RuntimePayloadTable<Generation, Work, Failure>,
{
    #[allow(
        clippy::indexing_slicing,
        reason = "the matching validated permit bitmap is the construction proof for this table coordinate"
    )]
    pub(crate) fn payload_slot(&self, index: SlotIndex) -> &PayloadSlot<Generation, Work, Failure> {
        // A `SlotIndex` reaches runtime code only from the matching permit/ready bitmap.
        // `RuntimeGeometry` validates that bitmap's capacity equals this selected table's length.
        &self.payloads.as_ref()[index.array_index()]
    }

    #[cfg(all(test, not(feature = "loom-model")))]
    #[allow(
        clippy::indexing_slicing,
        reason = "test coordinates are derived from the runtime's declared fixed capacity"
    )]
    pub(crate) fn test_payload_slot(
        &self,
        index: usize,
    ) -> &PayloadSlot<Generation, Work, Failure> {
        &self.payloads.as_ref()[index]
    }

    pub(crate) fn checked_out(&self) -> usize {
        let retired = self.work_permits.retired();
        let active_capacity = self.geometry.work_capacity() - retired;
        active_capacity - self.work_permits.available()
    }

    pub(crate) fn wake_ready_waiters(&self) {
        // The common completion path has no async waiter. Read only the compact armed word in
        // that case; work, terminal, and byte counters stay cold until a real waiter exists.
        let candidates = self.waiters.armed_snapshot();
        if candidates == 0 || self.work_permits.available() == 0 {
            return;
        }
        self.waiters
            .wake_fitting(candidates, self.byte_credits.available());
    }
}

fn allocate_runtime_identity() -> Result<RuntimeIdentity, RuntimeConfigError> {
    let mut observed = NEXT_RUNTIME_IDENTITY.load(Ordering::Acquire);
    loop {
        if observed == u64::MAX {
            return Err(RuntimeConfigError::RuntimeIdentityExhausted);
        }
        match NEXT_RUNTIME_IDENTITY.compare_exchange_weak(
            observed,
            observed + 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(identity) => return Ok(RuntimeIdentity(identity)),
            Err(actual) => observed = actual,
        }
    }
}

#[derive(Clone, Copy)]
enum GeometryAxis {
    Work,
    Waiter,
}

fn validate_slot_capacity(
    requested: usize,
    axis: GeometryAxis,
) -> Result<NonZeroU8, RuntimeConfigError> {
    if requested == 0 {
        return Err(match axis {
            GeometryAxis::Work => RuntimeConfigError::ZeroWorkSlots,
            GeometryAxis::Waiter => RuntimeConfigError::ZeroAsyncWaiters,
        });
    }
    if requested > MAX_WORK_SLOTS {
        return Err(match axis {
            GeometryAxis::Work => RuntimeConfigError::WorkSlotsExceedBitmap {
                requested,
                maximum: MAX_WORK_SLOTS,
            },
            GeometryAxis::Waiter => RuntimeConfigError::AsyncWaitersExceedBound {
                requested,
                maximum: MAX_WORK_SLOTS,
            },
        });
    }
    #[allow(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "requested is proven at most MAX_WORK_SLOTS, which is below u8::MAX"
    )]
    let value = requested as u8;
    NonZeroU8::new(value).ok_or(match axis {
        GeometryAxis::Work => RuntimeConfigError::ZeroWorkSlots,
        GeometryAxis::Waiter => RuntimeConfigError::ZeroAsyncWaiters,
    })
}

pub(crate) fn metrics<
    Generation,
    Work,
    Failure,
    AccountingPolicy,
    StoragePolicy: RuntimeStorage<Generation, Work, Failure>,
>(
    fabric: &Fabric<Generation, Work, Failure, AccountingPolicy, StoragePolicy>,
) -> RuntimeMetrics {
    let available_credits = fabric.byte_credits.available();
    let retired_work_slots = fabric.work_permits.retired();
    let active_capacity = fabric.geometry.work_capacity() - retired_work_slots;
    let available = fabric.work_permits.available();
    RuntimeMetrics {
        capacity: fabric.geometry.work_capacity(),
        active_capacity,
        available,
        checked_out: active_capacity - available,
        reserved_bytes: *fabric.budget.total - available_credits * *fabric.budget.quantum,
        retired_work_slots,
        terminal_occupied: fabric.terminal_ready.count(),
    }
}
