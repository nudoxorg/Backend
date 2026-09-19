//! Defines storage behavior for `backend_runtime::server`, whose purpose is to schedule bounded work with explicit credits, ownership, and wakeups.
//! This module owns the storage invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Monomorphic payload and waiter storage policies.
//!
//! Terminal facts stay in their permit-addressed payload cell until observed; there is no second
//! terminal-record queue. Remote storage reserves heap tables once, inline storage embeds exact
//! const-generic tables, and local storage borrows caller-provided `MaybeUninit` arenas. Hot paths
//! use only their selected concrete table type.
use alloc::{collections::TryReserveError, vec::Vec};
use core::{marker::PhantomData, mem::MaybeUninit};

use crate::server::{
    initialized_prefix::InitializedPrefix,
    payload_slot::PayloadSlot,
    runtime::{RuntimeConfigError, RuntimeGeometry},
    waiter::{InlineWaiterTable, LocalWaiterTable, RemoteWaiterTable, WaiterSlot, WaiterTable},
};

/// Selects one statically known retained-storage representation for a runtime.
///
/// This trait is sealed so the three policies retain their allocation and layout proofs. It is
/// public so APIs can remain generic over a selected policy without dynamic dispatch.
pub trait RuntimeStorage<Generation, Work, Failure>: private::Sealed {
    #[doc(hidden)]
    type Payloads: RuntimePayloadTable<Generation, Work, Failure>;
    #[doc(hidden)]
    type Waiters: WaiterTable;
}

/// Dynamic remote storage: all fixed-capacity tables reserve their heap allocation before the
/// first admission.
#[derive(Clone, Copy, Debug)]
pub enum RemoteStorage {}

/// Const-inline storage with separate physical work and async-waiter capacities.
#[derive(Clone, Copy, Debug, Default)]
pub struct InlineStorage<const WORK_SLOTS: usize, const WAITER_SLOTS: usize>;

/// Caller-arena storage. Construct it through [`LocalRuntimeArena`].
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalStorage<'arena>(PhantomData<&'arena mut ()>);

/// Heap-backed payload table used only by [`RemoteStorage`].
#[doc(hidden)]
pub struct RemotePayloads<Generation, Work, Failure> {
    slots: Vec<PayloadSlot<Generation, Work, Failure>>,
}

impl<Generation, Work, Failure> RemotePayloads<Generation, Work, Failure> {
    pub(crate) fn try_new(capacity: usize) -> Result<Self, TryReserveError> {
        let mut slots = Vec::new();
        slots.try_reserve_exact(capacity)?;
        for _ in 0..capacity {
            slots.push(PayloadSlot::new());
        }
        Ok(Self { slots })
    }
}

/// Const-inline payload table used only by [`InlineStorage`].
#[doc(hidden)]
pub struct InlinePayloads<Generation, Work, Failure, const CAPACITY: usize> {
    slots: [PayloadSlot<Generation, Work, Failure>; CAPACITY],
}

impl<Generation, Work, Failure, const CAPACITY: usize>
    InlinePayloads<Generation, Work, Failure, CAPACITY>
{
    pub(crate) fn new() -> Self {
        Self {
            slots: core::array::from_fn(|_| PayloadSlot::new()),
        }
    }
}

/// Caller-arena payload table used only by [`LocalStorage`].
#[doc(hidden)]
pub struct LocalPayloads<'arena, Generation, Work, Failure> {
    slots: InitializedPrefix<'arena, PayloadSlot<Generation, Work, Failure>>,
}

type PayloadArenaSlot<Generation, Work, Failure> =
    MaybeUninit<PayloadSlot<Generation, Work, Failure>>;
type PayloadArena<'arena, Generation, Work, Failure> =
    &'arena mut [PayloadArenaSlot<Generation, Work, Failure>];

impl<'arena, Generation, Work, Failure> LocalPayloads<'arena, Generation, Work, Failure> {
    pub(crate) fn initialize(slots: PayloadArena<'arena, Generation, Work, Failure>) -> Self {
        Self {
            slots: InitializedPrefix::initialize_with(slots, PayloadSlot::new),
        }
    }
}

/// Marker for a monomorphic payload table that borrows as its contiguous fixed slot slice.
#[doc(hidden)]
pub trait RuntimePayloadTable<Generation, Work, Failure>:
    AsRef<[PayloadSlot<Generation, Work, Failure>]>
{
}

impl<Generation, Work, Failure> RuntimePayloadTable<Generation, Work, Failure>
    for RemotePayloads<Generation, Work, Failure>
{
}

impl<Generation, Work, Failure> AsRef<[PayloadSlot<Generation, Work, Failure>]>
    for RemotePayloads<Generation, Work, Failure>
{
    fn as_ref(&self) -> &[PayloadSlot<Generation, Work, Failure>] {
        &self.slots
    }
}

impl<Generation, Work, Failure, const CAPACITY: usize>
    RuntimePayloadTable<Generation, Work, Failure>
    for InlinePayloads<Generation, Work, Failure, CAPACITY>
{
}

impl<Generation, Work, Failure, const CAPACITY: usize>
    AsRef<[PayloadSlot<Generation, Work, Failure>]>
    for InlinePayloads<Generation, Work, Failure, CAPACITY>
{
    fn as_ref(&self) -> &[PayloadSlot<Generation, Work, Failure>] {
        &self.slots
    }
}

impl<Generation, Work, Failure> RuntimePayloadTable<Generation, Work, Failure>
    for LocalPayloads<'_, Generation, Work, Failure>
{
}

impl<Generation, Work, Failure> AsRef<[PayloadSlot<Generation, Work, Failure>]>
    for LocalPayloads<'_, Generation, Work, Failure>
{
    fn as_ref(&self) -> &[PayloadSlot<Generation, Work, Failure>] {
        self.slots.as_slice()
    }
}

/// One caller-provided arena for local runtime storage.
///
/// Both slices must remain exclusively borrowed until the resulting runtime is dropped.
/// Work retained in those slices must live for the same arena lifetime; a shorter borrow cannot
/// be admitted into a longer-lived local runtime:
///
/// ```compile_fail
/// use core::convert::Infallible;
/// use backend_runtime::server::{
///     BoundedWork, ByteBudget, LocalRuntime, LocalRuntimeArena, RetainedBytes,
/// };
///
/// struct BorrowedWork<'value>(&'value u8);
/// impl BoundedWork for BorrowedWork<'_> {
///     fn retained_bytes(&self) -> RetainedBytes {
///         RetainedBytes::from(1)
///     }
/// }
///
/// fn reject_short_work(budget: ByteBudget) {
///     let mut payloads = [const { core::mem::MaybeUninit::uninit() }; 1];
///     let mut waiters = [const { core::mem::MaybeUninit::uninit() }; 1];
///     let arena = LocalRuntimeArena::<u8, BorrowedWork<'_>, Infallible>::new(
///         &mut payloads,
///         &mut waiters,
///     );
///     let mut runtime;
///     {
///         let value = 1;
///         let Ok(created) = LocalRuntime::from_arena(budget, arena) else { return };
///         runtime = created;
///         let (admission, _owner) = runtime.split();
///         let _admission = admission.admit(1, BorrowedWork(&value));
///     }
///     drop(runtime);
/// }
/// ```
pub struct LocalRuntimeArena<'arena, Generation, Work, Failure> {
    payloads: PayloadArena<'arena, Generation, Work, Failure>,
    waiters: &'arena mut [MaybeUninit<WaiterSlot>],
}

impl<'arena, Generation, Work, Failure> LocalRuntimeArena<'arena, Generation, Work, Failure> {
    /// Couples caller-owned uninitialized work and waiter memory into one scoped runtime arena.
    #[must_use]
    pub const fn new(
        payloads: PayloadArena<'arena, Generation, Work, Failure>,
        waiters: &'arena mut [MaybeUninit<WaiterSlot>],
    ) -> Self {
        Self { payloads, waiters }
    }

    pub(crate) fn geometry(&self) -> Result<RuntimeGeometry, RuntimeConfigError> {
        RuntimeGeometry::new(self.payloads.len(), self.waiters.len())
    }

    pub(crate) fn initialize(self) -> LocalStorageTables<'arena, Generation, Work, Failure> {
        LocalStorageTables {
            payloads: LocalPayloads::initialize(self.payloads),
            waiters: LocalWaiterTable::initialize(self.waiters),
        }
    }
}

/// Named initialized tables transferred from one validated caller arena into one local runtime.
pub(crate) struct LocalStorageTables<'arena, Generation, Work, Failure> {
    pub(crate) payloads: LocalPayloads<'arena, Generation, Work, Failure>,
    pub(crate) waiters: LocalWaiterTable<'arena>,
}

impl<Generation, Work, Failure> RuntimeStorage<Generation, Work, Failure> for RemoteStorage {
    type Payloads = RemotePayloads<Generation, Work, Failure>;
    type Waiters = RemoteWaiterTable;
}

impl<Generation, Work, Failure, const WORK_SLOTS: usize, const WAITER_SLOTS: usize>
    RuntimeStorage<Generation, Work, Failure> for InlineStorage<WORK_SLOTS, WAITER_SLOTS>
{
    type Payloads = InlinePayloads<Generation, Work, Failure, WORK_SLOTS>;
    type Waiters = InlineWaiterTable<WAITER_SLOTS>;
}

impl<'arena, Generation: 'arena, Work: 'arena, Failure: 'arena>
    RuntimeStorage<Generation, Work, Failure> for LocalStorage<'arena>
{
    type Payloads = LocalPayloads<'arena, Generation, Work, Failure>;
    type Waiters = LocalWaiterTable<'arena>;
}

mod private {
    pub trait Sealed {}
}

impl private::Sealed for RemoteStorage {}
impl<const WORK_SLOTS: usize, const WAITER_SLOTS: usize> private::Sealed
    for InlineStorage<WORK_SLOTS, WAITER_SLOTS>
{
}
impl private::Sealed for LocalStorage<'_> {}

#[cfg(all(test, not(feature = "loom-model")))]
mod tests {
    use core::{cell::Cell, mem::MaybeUninit};

    use crate::server::initialized_prefix::InitializedPrefix;

    struct DropProbe<'a>(&'a Cell<usize>);

    impl Drop for DropProbe<'_> {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    #[test]
    #[allow(
        clippy::manual_assert,
        clippy::panic,
        reason = "this adversarial constructor must unwind after exactly two initialized elements"
    )]
    fn local_payload_prefix_guard_drops_partial_initialization_during_unwind() {
        let drops = Cell::new(0);
        let mut slots = [const { MaybeUninit::<DropProbe<'_>>::uninit() }; 3];
        let mut built = 0_usize;
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _initialized = InitializedPrefix::initialize_with(&mut slots, || {
                if built == 2 {
                    panic!("simulate a future caller-arena element constructor failure");
                }
                built += 1;
                DropProbe(&drops)
            });
        }));
        assert!(unwind.is_err());
        assert_eq!(drops.get(), 2);
    }
}
