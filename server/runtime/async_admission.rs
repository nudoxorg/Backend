//! Defines async-admission behavior for `server-runtime`, whose purpose is to schedule bounded work with explicit credits, ownership, and wakeups.
//! This module owns the async-admission invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Executor-neutral future that waits for the ordinary bounded admission transition.
#![allow(
    missing_docs,
    reason = "the cancellation result is a small closed ownership vocabulary"
)]

use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use crate::storage::{RuntimePayloadTable, RuntimeStorage};
use crate::{
    Admission, AdmissionError, BoundedWork, WorkHandle, budget::credits_for, waiter::WaiterTicket,
};

enum AdmissionPhase<Generation, Work> {
    Waiting {
        generation: Generation,
        work: Work,
        ticket: WaiterTicket,
    },
    Ready(WorkHandle),
    Transitioning,
}

/// A non-cloneable pending admission. It owns caller work until ordinary admission succeeds or fails.
pub struct AdmissionFuture<
    'runtime,
    Generation,
    Work,
    AccountingPolicy = (),
    StoragePolicy: RuntimeStorage<Generation, Work, Failure> = crate::RemoteStorage,
    Failure = core::convert::Infallible,
> {
    admission: Admission<'runtime, Generation, Work, AccountingPolicy, StoragePolicy, Failure>,
    phase: AdmissionPhase<Generation, Work>,
}

/// Exact outcome of consuming an admission future for cancellation.
#[derive(Debug, Eq, PartialEq)]
pub enum AdmissionCancellation<Work> {
    Pending { work: Work },
    Completed,
}

impl<
    'runtime,
    Generation,
    Work,
    AccountingPolicy,
    StoragePolicy: RuntimeStorage<Generation, Work, Failure>,
    Failure,
> AdmissionFuture<'runtime, Generation, Work, AccountingPolicy, StoragePolicy, Failure>
{
    pub(crate) const fn new(
        admission: Admission<'runtime, Generation, Work, AccountingPolicy, StoragePolicy, Failure>,
        generation: Generation,
        work: Work,
        ticket: WaiterTicket,
    ) -> Self {
        Self {
            admission,
            phase: AdmissionPhase::Waiting {
                generation,
                work,
                ticket,
            },
        }
    }

    pub(crate) const fn ready(
        admission: Admission<'runtime, Generation, Work, AccountingPolicy, StoragePolicy, Failure>,
        handle: WorkHandle,
    ) -> Self {
        Self {
            admission,
            phase: AdmissionPhase::Ready(handle),
        }
    }

    /// Cancels waiting admission and returns the exact caller-owned work before any work credit moved.
    pub fn cancel(mut self) -> AdmissionCancellation<Work> {
        let phase = core::mem::replace(&mut self.phase, AdmissionPhase::Transitioning);
        match phase {
            AdmissionPhase::Waiting { work, ticket, .. } => {
                self.admission.fabric.waiters.release(&ticket);
                AdmissionCancellation::Pending { work }
            }
            AdmissionPhase::Ready(_) | AdmissionPhase::Transitioning => {
                AdmissionCancellation::Completed
            }
        }
    }
}

impl<
    Generation: Copy + Unpin,
    Work: BoundedWork + Unpin,
    AccountingPolicy: crate::metrics::Accounting,
    StoragePolicy: RuntimeStorage<Generation, Work, Failure>,
    Failure,
> Future for AdmissionFuture<'_, Generation, Work, AccountingPolicy, StoragePolicy, Failure>
where
    StoragePolicy::Payloads: RuntimePayloadTable<Generation, Work, Failure>,
{
    type Output = Result<WorkHandle, AdmissionError<Work>>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let future = self.as_mut().get_mut();
        if let AdmissionPhase::Ready(handle) = future.phase {
            return Poll::Ready(Ok(handle));
        }
        match future.try_admit() {
            Ok(Some(handle)) => return Poll::Ready(Ok(handle)),
            Err(error) => return Poll::Ready(Err(error)),
            Ok(None) => {}
        }
        match &future.phase {
            AdmissionPhase::Waiting { ticket, .. } => {
                future.admission.fabric.waiters.arm(ticket, context.waker());
            }
            AdmissionPhase::Ready(handle) => return Poll::Ready(Ok(*handle)),
            AdmissionPhase::Transitioning => return Poll::Pending,
        }
        // Register → arm → recheck closes the return-between-observation-and-sleep window.
        match future.try_admit() {
            Ok(Some(handle)) => Poll::Ready(Ok(handle)),
            Err(error) => Poll::Ready(Err(error)),
            Ok(None) => Poll::Pending,
        }
    }
}

impl<
    Generation: Copy,
    Work: BoundedWork,
    AccountingPolicy: crate::metrics::Accounting,
    StoragePolicy: RuntimeStorage<Generation, Work, Failure>,
    Failure,
> AdmissionFuture<'_, Generation, Work, AccountingPolicy, StoragePolicy, Failure>
where
    StoragePolicy::Payloads: RuntimePayloadTable<Generation, Work, Failure>,
{
    fn try_admit(&mut self) -> Result<Option<WorkHandle>, AdmissionError<Work>> {
        let phase = core::mem::replace(&mut self.phase, AdmissionPhase::Transitioning);
        let AdmissionPhase::Waiting {
            generation,
            work,
            ticket,
        } = phase
        else {
            self.phase = phase;
            return Ok(None);
        };
        match self.admission.admit(generation, work) {
            Ok(handle) => {
                self.admission.fabric.waiters.release(&ticket);
                self.phase = AdmissionPhase::Ready(handle);
                Ok(Some(handle))
            }
            Err(AdmissionError::Rejected { rejected }) => {
                self.phase = AdmissionPhase::Waiting {
                    generation,
                    work: rejected.work,
                    ticket,
                };
                Ok(None)
            }
            Err(error) => {
                self.admission.fabric.waiters.release(&ticket);
                Err(error)
            }
        }
    }
}

impl<
    Generation,
    Work,
    AccountingPolicy,
    StoragePolicy: RuntimeStorage<Generation, Work, Failure>,
    Failure,
> Drop for AdmissionFuture<'_, Generation, Work, AccountingPolicy, StoragePolicy, Failure>
{
    fn drop(&mut self) {
        if let AdmissionPhase::Waiting { ticket, .. } = &self.phase {
            self.admission.fabric.waiters.release(ticket);
        }
    }
}

pub(crate) fn register_future<
    Generation: Copy,
    Work: BoundedWork,
    AccountingPolicy: crate::metrics::Accounting,
    StoragePolicy: RuntimeStorage<Generation, Work, Failure>,
    Failure,
>(
    admission: Admission<'_, Generation, Work, AccountingPolicy, StoragePolicy, Failure>,
    generation: Generation,
    work: Work,
) -> crate::AdmissionWaitResult<'_, Generation, Work, AccountingPolicy, StoragePolicy, Failure>
where
    StoragePolicy::Payloads: RuntimePayloadTable<Generation, Work, Failure>,
{
    let credits = credits_for(work.retained_bytes(), admission.fabric.budget.quantum);
    let ticket = match admission.fabric.waiters.register(credits) {
        Ok(ticket) => ticket,
        Err(crate::WaiterRegistrationError::Capacity) => {
            return Err(AdmissionError::WaiterCapacity { work });
        }
        Err(source) => return Err(AdmissionError::WaiterRegistration { work, source }),
    };
    Ok(AdmissionFuture::new(admission, generation, work, ticket))
}

#[cfg(all(test, not(feature = "loom-model")))]
mod tests {
    use core::{
        pin::Pin,
        sync::atomic::{AtomicUsize, Ordering},
        task::{Context, Poll, Waker},
    };
    use std::{sync::Arc, task::Wake};

    use thiserror::Error;

    use crate::{
        AdmissionCancellation, AdmissionError, BoundedWork, ByteBudget, ByteBudgetError,
        ByteQuantum, RemoteRuntime, RetainedBytes, RuntimeConfigError,
    };

    use super::AdmissionFuture;

    #[derive(Debug, Eq, PartialEq)]
    struct Work {
        id: u8,
        bytes: RetainedBytes,
    }

    impl BoundedWork for Work {
        fn retained_bytes(&self) -> RetainedBytes {
            self.bytes
        }
    }

    struct WakeCount(AtomicUsize);
    impl Wake for WakeCount {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::Release);
        }
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum AsyncStep {
        InitialLarge,
        InitialSmall,
        LargeAfterWake,
        SmallAfterLarge,
        SmallAfterWake,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum AsyncObserved {
        Pending,
        Ready,
    }

    #[derive(Debug, Error)]
    enum AsyncTestError {
        #[error("byte budget setup failed")]
        Budget(#[from] ByteBudgetError),
        #[error("runtime setup failed")]
        Runtime(#[from] RuntimeConfigError),
        #[error("runtime owner containment failed")]
        Owner(#[from] crate::OwnerFault),
        #[error("admission failed")]
        Admission(#[from] AdmissionError<Work>),
        #[error("poll {step:?} expected {expected:?}, observed {observed:?}")]
        Poll {
            step: AsyncStep,
            expected: AsyncObserved,
            observed: AsyncObserved,
        },
        #[error("wake count at {step:?} expected {expected}, observed {observed}")]
        Wakes {
            step: AsyncStep,
            expected: usize,
            observed: usize,
        },
        #[error("owner had no queued work at {step:?}")]
        NoQueuedWork { step: AsyncStep },
        #[error("owner terminal missing at {step:?}")]
        MissingTerminal { step: AsyncStep },
        #[error("cancel returned an unexpected pending work item")]
        CancelledWork {
            observed: AdmissionCancellation<Work>,
        },
    }

    #[test]
    fn multi_waiter_register_arm_recheck_wakes_every_fitting_byte_request()
    -> Result<(), AsyncTestError> {
        let budget = ByteBudget::new(RetainedBytes::from(4), ByteQuantum::try_from(1)?)?;
        let mut runtime = RemoteRuntime::<u8, Work>::with_waiter_capacity(1, budget, 2)?;
        let (admission, mut owner) = runtime.split();
        let mut scenario = TwoWaiters::enqueue(admission)?;
        scenario.observe_initial()?;
        execute_and_observe(&mut owner, AsyncStep::LargeAfterWake)?;
        scenario.observe_first_wake()?;
        execute_and_observe(&mut owner, AsyncStep::SmallAfterWake)?;
        scenario.observe_second_wake()
    }

    struct TwoWaiters<'runtime> {
        first: crate::WorkHandle,
        large: AdmissionFuture<'runtime, u8, Work>,
        small: AdmissionFuture<'runtime, u8, Work>,
        large_wakes: Arc<WakeCount>,
        small_wakes: Arc<WakeCount>,
    }

    impl<'runtime> TwoWaiters<'runtime> {
        fn enqueue(
            admission: crate::Admission<'runtime, u8, Work>,
        ) -> Result<Self, AsyncTestError> {
            let first = admission.admit(1, work(0, 1))?;
            Ok(Self {
                first,
                large: admission.admit_when_ready(1, work(1, 4))?,
                small: admission.admit_when_ready(1, work(2, 1))?,
                large_wakes: Arc::new(WakeCount(AtomicUsize::new(0))),
                small_wakes: Arc::new(WakeCount(AtomicUsize::new(0))),
            })
        }

        fn observe_initial(&mut self) -> Result<(), AsyncTestError> {
            let large_waker = self.large_waker();
            expect_pending(AsyncStep::InitialLarge, &mut self.large, &large_waker)?;
            let small_waker = self.small_waker();
            expect_pending(AsyncStep::InitialSmall, &mut self.small, &small_waker)
        }

        fn observe_first_wake(&mut self) -> Result<(), AsyncTestError> {
            expect_wakes(AsyncStep::LargeAfterWake, &self.large_wakes, 1)?;
            expect_wakes(AsyncStep::SmallAfterWake, &self.small_wakes, 1)?;
            let large_waker = self.large_waker();
            let handle = expect_ready(AsyncStep::LargeAfterWake, &mut self.large, &large_waker)?;
            if handle == self.first {
                return Err(AsyncTestError::Poll {
                    step: AsyncStep::LargeAfterWake,
                    expected: AsyncObserved::Ready,
                    observed: AsyncObserved::Pending,
                });
            }
            let small_waker = self.small_waker();
            expect_pending(AsyncStep::SmallAfterLarge, &mut self.small, &small_waker)
        }

        fn observe_second_wake(&mut self) -> Result<(), AsyncTestError> {
            expect_wakes(AsyncStep::SmallAfterWake, &self.small_wakes, 2)?;
            let small_waker = self.small_waker();
            let _ = expect_ready(AsyncStep::SmallAfterWake, &mut self.small, &small_waker)?;
            Ok(())
        }

        fn large_waker(&self) -> Waker {
            Waker::from(Arc::clone(&self.large_wakes))
        }

        fn small_waker(&self) -> Waker {
            Waker::from(Arc::clone(&self.small_wakes))
        }
    }

    fn work(id: u8, bytes: usize) -> Work {
        Work {
            id,
            bytes: RetainedBytes::from(bytes),
        }
    }

    #[test]
    fn cancellation_reclaims_a_waiter_slot_and_returns_its_exact_work() -> Result<(), AsyncTestError>
    {
        let budget = ByteBudget::new(RetainedBytes::from(1), ByteQuantum::try_from(1)?)?;
        let mut runtime = RemoteRuntime::<u8, Work>::new(1, budget)?;
        let (admission, _) = runtime.split();
        let _first = admission.admit(
            1,
            Work {
                id: 0,
                bytes: RetainedBytes::from(1),
            },
        )?;
        let cancelled = admission.admit_when_ready(
            1,
            Work {
                id: 7,
                bytes: RetainedBytes::from(1),
            },
        )?;
        match cancelled.cancel() {
            AdmissionCancellation::Pending {
                work: Work { id: 7, bytes },
            } if bytes == RetainedBytes::from(1) => {}
            observed => return Err(AsyncTestError::CancelledWork { observed }),
        }
        let replacement = admission.admit_when_ready(
            1,
            Work {
                id: 8,
                bytes: RetainedBytes::from(1),
            },
        );
        match replacement {
            Ok(_) => Ok(()),
            Err(error) => Err(AsyncTestError::Admission(error)),
        }
    }

    #[test]
    fn full_waiter_table_never_blocks_an_immediately_admissible_work_item()
    -> Result<(), AsyncTestError> {
        let budget = ByteBudget::new(RetainedBytes::from(2), ByteQuantum::try_from(1)?)?;
        let mut runtime = RemoteRuntime::<u8, Work>::with_waiter_capacity(2, budget, 1)?;
        let (admission, mut owner) = runtime.split();
        let _blocking = admission.admit(
            1,
            Work {
                id: 0,
                bytes: RetainedBytes::from(2),
            },
        )?;
        let mut waiting = admission.admit_when_ready(
            1,
            Work {
                id: 1,
                bytes: RetainedBytes::from(1),
            },
        )?;
        let waker = Waker::from(Arc::new(WakeCount(AtomicUsize::new(0))));
        expect_pending(AsyncStep::InitialSmall, &mut waiting, &waker)?;
        execute_and_observe(&mut owner, AsyncStep::SmallAfterWake)?;
        let mut immediate = admission.admit_when_ready(
            1,
            Work {
                id: 2,
                bytes: RetainedBytes::from(1),
            },
        )?;
        let _handle = expect_ready(AsyncStep::SmallAfterWake, &mut immediate, &waker)?;
        Ok(())
    }

    #[test]
    fn pending_admission_future_is_send_and_sync_when_its_work_is() {
        fn assert_send_sync<Type: Send + Sync>() {}
        assert_send_sync::<AdmissionFuture<'static, u8, Work>>();
    }

    fn expect_pending(
        step: AsyncStep,
        future: &mut AdmissionFuture<'_, u8, Work>,
        waker: &Waker,
    ) -> Result<(), AsyncTestError> {
        let mut context = Context::from_waker(waker);
        match Pin::new(future).poll(&mut context) {
            Poll::Pending => Ok(()),
            Poll::Ready(Ok(_) | Err(_)) => Err(AsyncTestError::Poll {
                step,
                expected: AsyncObserved::Pending,
                observed: AsyncObserved::Ready,
            }),
        }
    }

    fn expect_ready(
        step: AsyncStep,
        future: &mut AdmissionFuture<'_, u8, Work>,
        waker: &Waker,
    ) -> Result<crate::WorkHandle, AsyncTestError> {
        let mut context = Context::from_waker(waker);
        match Pin::new(future).poll(&mut context) {
            Poll::Ready(Ok(handle)) => Ok(handle),
            Poll::Ready(Err(error)) => Err(AsyncTestError::Admission(error)),
            Poll::Pending => Err(AsyncTestError::Poll {
                step,
                expected: AsyncObserved::Ready,
                observed: AsyncObserved::Pending,
            }),
        }
    }

    fn execute_and_observe(
        owner: &mut crate::Owner<'_, u8, Work>,
        step: AsyncStep,
    ) -> Result<(), AsyncTestError> {
        if owner.execute_next(&1, |_| Ok(()))? == crate::OwnerProgress::Idle {
            return Err(AsyncTestError::NoQueuedWork { step });
        }
        match owner.poll_terminal()? {
            Some(_) => Ok(()),
            None => Err(AsyncTestError::MissingTerminal { step }),
        }
    }

    fn expect_wakes(
        step: AsyncStep,
        counter: &WakeCount,
        expected: usize,
    ) -> Result<(), AsyncTestError> {
        let observed = counter.0.load(Ordering::Acquire);
        if observed == expected {
            Ok(())
        } else {
            Err(AsyncTestError::Wakes {
                step,
                expected,
                observed,
            })
        }
    }
}
