use core::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use thiserror::Error;

use crate::{
    AdmissionError, AtomicAccounting, BoundedWork, ByteBudget, ByteBudgetError, ByteQuantum,
    CancelResult, InlineRuntime, LocalRuntime, LocalRuntimeArena, RemoteRuntime, RetainedBytes,
    Runtime, RuntimeConfigError, TerminalOutcome,
};

#[derive(Debug, Eq, PartialEq)]
struct Work {
    bytes: RetainedBytes,
}

type ObservedRuntime<Generation, Work, Failure = core::convert::Infallible> =
    Runtime<Generation, Work, Failure, AtomicAccounting, crate::RemoteStorage>;

#[derive(Debug)]
struct PanicWork {
    bytes: RetainedBytes,
    drops: std::sync::Arc<core::sync::atomic::AtomicUsize>,
    panic_on_drop: bool,
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("callback rejected the owned work with code {code}")]
struct CallbackFailure {
    code: u8,
}

impl BoundedWork for PanicWork {
    fn retained_bytes(&self) -> RetainedBytes {
        self.bytes
    }
}

impl Drop for PanicWork {
    #[allow(
        clippy::manual_assert,
        clippy::panic,
        reason = "this test fixture deliberately simulates an untrusted Work destructor"
    )]
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::Release);
        if self.panic_on_drop {
            panic!("adversarial work destructor panic");
        }
    }
}

#[derive(Clone, Copy)]
struct ConcurrentScenario {
    offered: u8,
    cancel_every: u8,
}
const SUSTAINED_CANCELLATION: ConcurrentScenario = ConcurrentScenario {
    offered: 80,
    cancel_every: 2,
};
impl BoundedWork for Work {
    fn retained_bytes(&self) -> RetainedBytes {
        self.bytes
    }
}

#[derive(Debug, Error)]
enum TestError {
    #[error("byte budget setup failed")]
    Budget(#[from] ByteBudgetError),
    #[error("runtime setup failed")]
    Runtime(#[from] RuntimeConfigError),
    #[error("direct admission failed")]
    Admission(#[from] AdmissionError<Work>),
    #[error("runtime owner containment failed")]
    Owner(#[from] crate::OwnerFault),
    #[error("retired-slot scenario observed an unexpected admission result")]
    RetirementAdmission(AdmissionError<Work>),
    #[error("producer rejected an impossible admission state")]
    Producer(#[from] ProducerFault),
    #[error("the scoped producer thread panicked before reporting its typed outcome")]
    ProducerThreadPanicked(#[source] crate::test_report::ThreadPanic),
    #[error("the terminal reserve did not contain an event after successful execution")]
    MissingTerminal,
    #[error("terminal counter {counter:?} cannot represent the observed terminal report")]
    TerminalCount {
        counter: TerminalCounter,
        #[source]
        source: core::num::TryFromIntError,
    },
}

#[derive(Debug, Error)]
enum PanicScenarioError {
    #[error("byte budget setup failed")]
    Budget(#[from] ByteBudgetError),
    #[error("runtime setup failed")]
    Runtime(#[from] RuntimeConfigError),
    #[error("admission failed")]
    Admission(#[from] AdmissionError<PanicWork>),
    #[error("runtime owner containment failed")]
    Owner(#[from] crate::OwnerFault),
    #[error("callback panic was not propagated")]
    MissingCallbackPanic,
    #[error("work destructor panic was not propagated")]
    MissingDropPanic,
    #[error("missing terminal after committed panic path")]
    MissingTerminal,
}

#[derive(Clone, Copy, Debug)]
enum TerminalCounter {
    Cancelled,
    Completed,
}

#[derive(Debug, Error)]
enum ProducerFault {
    #[error("a retired slot rejected producer work")]
    SlotRetired {
        work: Work,
        #[source]
        source: crate::SlotClaimError,
    },
    #[error("synchronous admission unexpectedly returned async waiter capacity")]
    WaiterCapacity { work: Work },
    #[error("synchronous admission unexpectedly returned async waiter registration")]
    WaiterRegistration {
        work: Work,
        #[source]
        source: crate::WaiterRegistrationError,
    },
    #[error("cancel attempt {sequence} observed {observed:?}, not a live queued transition")]
    CancelTransition {
        sequence: u8,
        observed: CancelResult,
    },
}

#[derive(Default)]
struct ProducerReport {
    admitted: usize,
    rejected_work_slots: usize,
    rejected_bytes: usize,
    cancel_attempts: usize,
    cancelled: usize,
    not_queued: usize,
}

#[derive(Default)]
struct TerminalReport {
    completed: usize,
    cancelled: usize,
    failed: usize,
    stale: usize,
}

impl TerminalReport {
    const fn record<Failure>(&mut self, outcome: &TerminalOutcome<Failure>) {
        match outcome {
            TerminalOutcome::Completed => self.completed += 1,
            TerminalOutcome::Failed { .. } | TerminalOutcome::ExecutorUnwound => {
                self.failed += 1;
            }
            TerminalOutcome::Cancelled => self.cancelled += 1,
            TerminalOutcome::StaleGeneration => self.stale += 1,
        }
    }

    const fn total(&self) -> usize {
        self.completed + self.cancelled + self.failed + self.stale
    }
}

fn budget(bytes: usize) -> Result<ByteBudget, ByteBudgetError> {
    ByteBudget::new(RetainedBytes::from(bytes), ByteQuantum::try_from(1)?)
}

#[test]
fn accounting_policy_is_static_and_default_runtime_carries_no_counter_storage()
-> Result<(), TestError> {
    use core::mem::size_of;

    assert_eq!(size_of::<()>(), 0);
    assert_eq!(size_of::<AtomicAccounting>(), 56);
    assert_eq!(size_of::<crate::WorkHandle>(), 16);
    assert_eq!(
        size_of::<ObservedRuntime<u8, Work>>(),
        size_of::<RemoteRuntime<u8, Work>>() + size_of::<AtomicAccounting>()
    );

    let mut runtime = RemoteRuntime::<u8, Work>::new(1, budget(1)?)?;
    let (admission, mut owner) = runtime.split();
    admission.admit(
        1,
        Work {
            bytes: RetainedBytes::from(1),
        },
    )?;
    let live = crate::runtime::metrics(owner.fabric);
    assert_eq!(live.checked_out, 1);
    assert_eq!(live.reserved_bytes, 1);
    assert_eq!(
        owner.execute_next(&1, |_| Ok(()))?,
        crate::OwnerProgress::Terminalized
    );
    let _terminal = owner.poll_terminal()?.ok_or(TestError::MissingTerminal)?;
    let metrics = crate::runtime::metrics(owner.fabric);
    assert_eq!(metrics.checked_out, 0);
    Ok(())
}

#[test]
fn terminal_retention_is_in_place_and_inline_or_local_storage_needs_no_heap_owner()
-> Result<(), TestError> {
    use core::mem::size_of;

    let inline_one = size_of::<InlineRuntime<u8, [u8; 1], 1, 1>>();
    let inline_four = size_of::<InlineRuntime<u8, [u8; 1], 4, 4>>();
    let inline_sixty_four = size_of::<InlineRuntime<u8, [u8; 1], 64, 64>>();
    let large_inline_sixty_four = size_of::<InlineRuntime<u8, [u8; 128], 64, 64>>();
    assert!(inline_one < inline_four && inline_four < inline_sixty_four);
    assert!(inline_sixty_four < large_inline_sixty_four);
    let mut inline = InlineRuntime::<u8, Work, 1, 1>::new(budget(1)?)?;
    let (admission, mut owner) = inline.split();
    admission.admit(
        1,
        Work {
            bytes: RetainedBytes::from(1),
        },
    )?;
    assert_eq!(
        owner.execute_next(&1, |_| Ok(()))?,
        crate::OwnerProgress::Terminalized
    );
    match admission.admit(
        1,
        Work {
            bytes: RetainedBytes::from(1),
        },
    ) {
        Err(AdmissionError::Rejected { rejected }) => {
            assert_eq!(rejected.reason, crate::RejectionReason::WorkSlots);
        }
        Err(error) => return Err(TestError::RetirementAdmission(error)),
        Ok(_) => return Err(TestError::MissingTerminal),
    }
    let _terminal = owner.poll_terminal()?.ok_or(TestError::MissingTerminal)?;
    admission.admit(
        1,
        Work {
            bytes: RetainedBytes::from(1),
        },
    )?;

    let mut payloads = [const { core::mem::MaybeUninit::uninit() }; 1];
    let mut waiters = [const { core::mem::MaybeUninit::uninit() }; 1];
    let arena = LocalRuntimeArena::new(&mut payloads, &mut waiters);
    let mut local = LocalRuntime::<u8, Work>::from_arena(budget(1)?, arena)?;
    let (admission, mut owner) = local.split();
    admission.admit(
        1,
        Work {
            bytes: RetainedBytes::from(1),
        },
    )?;
    assert_eq!(
        owner.execute_next(&1, |_| Ok(()))?,
        crate::OwnerProgress::Terminalized
    );
    let _terminal = owner.poll_terminal()?.ok_or(TestError::MissingTerminal)?;
    Ok(())
}

#[test]
fn payload_cell_lifecycle_covers_cancel_terminal_reuse_and_drop() -> Result<(), PanicScenarioError>
{
    let drops = std::sync::Arc::new(core::sync::atomic::AtomicUsize::new(0));
    {
        let mut runtime = InlineRuntime::<u8, PanicWork, 1, 1>::new(budget(1)?)?;
        {
            let (admission, mut owner) = runtime.split();
            let cancelled = admission.admit(
                1,
                PanicWork {
                    bytes: RetainedBytes::from(1),
                    drops: std::sync::Arc::clone(&drops),
                    panic_on_drop: false,
                },
            )?;
            assert_eq!(admission.cancel_queued(cancelled), CancelResult::Cancelled);
            assert_eq!(
                owner.execute_next(&1, |_| Ok(()))?,
                crate::OwnerProgress::Terminalized
            );
            let terminal = owner
                .poll_terminal()?
                .ok_or(PanicScenarioError::MissingTerminal)?;
            assert_eq!(terminal.outcome, TerminalOutcome::Cancelled);

            // The same coordinate now stores a second terminal enum value. Leave it
            // unpolled so runtime drop exercises the terminal-field destructor path.
            admission.admit(
                1,
                PanicWork {
                    bytes: RetainedBytes::from(1),
                    drops: std::sync::Arc::clone(&drops),
                    panic_on_drop: false,
                },
            )?;
            assert_eq!(
                owner.execute_next(&1, |_| Ok(()))?,
                crate::OwnerProgress::Terminalized
            );
        }
    }
    assert_eq!(drops.load(Ordering::Acquire), 2);

    // A queued payload takes the other safe enum branch at fabric drop.
    {
        let mut runtime = InlineRuntime::<u8, PanicWork, 1, 1>::new(budget(1)?)?;
        let (admission, _owner) = runtime.split();
        admission.admit(
            1,
            PanicWork {
                bytes: RetainedBytes::from(1),
                drops: std::sync::Arc::clone(&drops),
                panic_on_drop: false,
            },
        )?;
    }
    assert_eq!(drops.load(Ordering::Acquire), 3);
    Ok(())
}

#[test]
fn scoped_concurrent_admission_cancellation_and_owner_drain_conserve_every_credit()
-> Result<(), TestError> {
    assert_sustained_outcome(run_sustained_outcome()?)
}

struct SustainedOutcome {
    metrics: crate::RuntimeMetrics,
    history: crate::RuntimeHistory,
    producer: ProducerReport,
    terminal: TerminalReport,
}

fn run_sustained_outcome() -> Result<SustainedOutcome, TestError> {
    let mut runtime = ObservedRuntime::<u8, Work>::new(4, budget(16)?)?;
    let (admission, mut owner) = runtime.split();
    let producers_finished = AtomicBool::new(false);
    let (producer, terminal) = thread::scope(
        |scope| -> Result<(ProducerReport, TerminalReport), TestError> {
            let producer = admission;
            let done = &producers_finished;
            let submit = scope.spawn(move || run_producer(producer, done, SUSTAINED_CANCELLATION));
            let mut terminal_report = TerminalReport::default();
            while !producers_finished.load(Ordering::Acquire) || runtime_metrics_live(&owner) {
                if owner.execute_next(&1, |_| Ok(()))? == crate::OwnerProgress::Idle {
                    thread::yield_now();
                }
                while let Some(terminal) = owner.poll_terminal()? {
                    terminal_report.record(&terminal.outcome);
                }
            }
            while let Some(terminal) = owner.poll_terminal()? {
                terminal_report.record(&terminal.outcome);
            }
            let producer_report = crate::test_report::join(submit.join())
                .map_err(TestError::ProducerThreadPanicked)??;
            Ok((producer_report, terminal_report))
        },
    )?;
    Ok(SustainedOutcome {
        metrics: runtime.metrics(),
        history: runtime.history(),
        producer,
        terminal,
    })
}

fn assert_sustained_outcome(outcome: SustainedOutcome) -> Result<(), TestError> {
    let SustainedOutcome {
        metrics,
        history,
        producer,
        terminal,
    } = outcome;
    assert_producer(&producer);
    assert_terminal(&producer, &terminal);
    assert_metrics(metrics);
    assert_history_matches(history, &producer, &terminal)
}

fn assert_producer(producer: &ProducerReport) {
    assert_eq!(
        producer.admitted,
        usize::from(SUSTAINED_CANCELLATION.offered)
    );
    assert_eq!(
        producer.cancel_attempts,
        usize::from(SUSTAINED_CANCELLATION.offered / SUSTAINED_CANCELLATION.cancel_every)
    );
    assert_eq!(
        producer.cancelled + producer.not_queued,
        producer.cancel_attempts
    );
}

fn assert_terminal(producer: &ProducerReport, terminal: &TerminalReport) {
    assert_eq!(terminal.total(), producer.admitted);
    assert_eq!(terminal.cancelled, producer.cancelled);
    assert_eq!(
        terminal.completed,
        producer.not_queued + producer.admitted - producer.cancel_attempts
    );
    assert_eq!(terminal.failed, 0);
    assert_eq!(terminal.stale, 0);
}

fn assert_metrics(metrics: crate::RuntimeMetrics) {
    assert_eq!(metrics.available, metrics.capacity);
    assert_eq!(metrics.checked_out, 0);
    assert_eq!(metrics.reserved_bytes, 0);
    assert_eq!(metrics.terminal_occupied, 0);
}

fn assert_history_matches(
    history: crate::RuntimeHistory,
    producer: &ProducerReport,
    terminal: &TerminalReport,
) -> Result<(), TestError> {
    assert_eq!(
        history_count(history.cancelled, TerminalCounter::Cancelled)?,
        terminal.cancelled
    );
    assert_eq!(
        history_count(history.completed, TerminalCounter::Completed)?,
        terminal.completed
    );
    assert_eq!(
        history_count(history.rejected_work_slots, TerminalCounter::Completed)?,
        producer.rejected_work_slots
    );
    assert_eq!(
        history_count(history.rejected_byte_budget, TerminalCounter::Completed)?,
        producer.rejected_bytes
    );
    Ok(())
}

fn history_count(value: u64, counter: TerminalCounter) -> Result<usize, TestError> {
    usize::try_from(value).map_err(|source| TestError::TerminalCount { counter, source })
}

fn runtime_metrics_live(
    owner: &crate::Owner<'_, u8, Work, core::convert::Infallible, AtomicAccounting>,
) -> bool {
    owner.fabric.checked_out() != 0
}

fn run_producer(
    admission: crate::Admission<'_, u8, Work, AtomicAccounting>,
    done: &AtomicBool,
    scenario: ConcurrentScenario,
) -> Result<ProducerReport, ProducerFault> {
    let mut report = ProducerReport::default();
    for sequence in 0..scenario.offered {
        let mut work = Work {
            bytes: RetainedBytes::from(1),
        };
        loop {
            match admission.admit(1, work) {
                Ok(handle) => {
                    report.admitted += 1;
                    if sequence.is_multiple_of(scenario.cancel_every) {
                        report.cancel_attempts += 1;
                        match admission.cancel_queued(handle) {
                            CancelResult::Cancelled => report.cancelled += 1,
                            CancelResult::NotQueued => report.not_queued += 1,
                            observed => {
                                return Err(ProducerFault::CancelTransition { sequence, observed });
                            }
                        }
                    }
                    break;
                }
                Err(AdmissionError::Rejected { rejected }) => {
                    work = rejected.work;
                    match rejected.reason {
                        crate::RejectionReason::WorkSlots => report.rejected_work_slots += 1,
                        crate::RejectionReason::ByteBudget => report.rejected_bytes += 1,
                    }
                    thread::yield_now();
                }
                Err(AdmissionError::SlotRetired { work, source }) => {
                    return Err(ProducerFault::SlotRetired { work, source });
                }
                Err(AdmissionError::WaiterCapacity { work }) => {
                    return Err(ProducerFault::WaiterCapacity { work });
                }
                Err(AdmissionError::WaiterRegistration { work, source }) => {
                    return Err(ProducerFault::WaiterRegistration { work, source });
                }
            }
        }
    }
    done.store(true, Ordering::Release);
    Ok(report)
}

#[test]
fn runtime_identity_rejects_cross_runtime_handles_and_one_work_can_reserve_many_credits()
-> Result<(), TestError> {
    let mut first = RemoteRuntime::<u8, Work>::new(1, budget(128)?)?;
    let mut second = RemoteRuntime::<u8, Work>::new(1, budget(128)?)?;
    let (first_admission, mut first_owner) = first.split();
    let (second_admission, _) = second.split();
    let handle = first_admission.admit(
        1,
        Work {
            bytes: RetainedBytes::from(128),
        },
    )?;
    assert_eq!(
        second_admission.cancel_queued(handle),
        CancelResult::ForeignRuntime
    );
    let executed = first_owner.execute_next(&1, |_| Ok(()))?;
    assert_eq!(executed, crate::OwnerProgress::Terminalized);
    let terminal = first_owner
        .poll_terminal()?
        .ok_or(TestError::MissingTerminal)?;
    assert_eq!(terminal.handle, handle);
    assert_eq!(terminal.outcome, TerminalOutcome::Completed);
    Ok(())
}

#[test]
fn exhausted_slot_is_retired_while_a_healthy_slot_keeps_progressing() -> Result<(), TestError> {
    let mut runtime = ObservedRuntime::<u8, Work>::new(2, budget(2)?)?;
    #[allow(
        clippy::indexing_slicing,
        reason = "the two-slot test fixture addresses its declared first physical slot"
    )]
    runtime.fabric.test_payload_slot(0).exhaust_free_for_test();
    let (admission, mut owner) = runtime.split();
    match admission.admit(
        1,
        Work {
            bytes: RetainedBytes::from(1),
        },
    ) {
        Err(AdmissionError::SlotRetired { work, source }) => {
            assert_eq!(work.bytes, RetainedBytes::from(1));
            assert_eq!(
                source,
                crate::SlotClaimError::EpochExhausted {
                    epoch: crate::slot::MAX_EPOCH
                }
            );
        }
        Err(error) => return Err(TestError::RetirementAdmission(error)),
        Ok(_) => return Err(TestError::MissingTerminal),
    }
    let progressed = admission.admit(
        1,
        Work {
            bytes: RetainedBytes::from(1),
        },
    )?;
    let ran = owner.execute_next(&1, |_| Ok(()))?;
    assert_eq!(ran, crate::OwnerProgress::Terminalized);
    let terminal = owner.poll_terminal()?.ok_or(TestError::MissingTerminal)?;
    assert_eq!(terminal.handle, progressed);
    let metrics = runtime.metrics();
    assert_eq!(metrics.retired_work_slots, 1);
    assert_eq!(metrics.capacity, 2);
    assert_eq!(metrics.active_capacity, 1);
    assert_eq!(metrics.available, 1);
    assert_eq!(metrics.checked_out, 0);
    Ok(())
}

#[test]
#[allow(
    clippy::panic,
    reason = "this test deliberately starts the callback unwind CompletionGuard must close"
)]
fn callback_unwind_commits_executor_unwound_before_the_original_panic_continues()
-> Result<(), PanicScenarioError> {
    let drops = std::sync::Arc::new(core::sync::atomic::AtomicUsize::new(0));
    let mut runtime = ObservedRuntime::<u8, PanicWork, CallbackFailure>::new(1, budget(1)?)?;
    let (admission, mut owner) = runtime.split();
    admission.admit(
        1,
        PanicWork {
            bytes: RetainedBytes::from(1),
            drops: std::sync::Arc::clone(&drops),
            panic_on_drop: false,
        },
    )?;
    let callback = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _outcome = owner.execute_next(&1, |_| -> Result<(), CallbackFailure> {
            panic!("adversarial callback panic");
        });
    }));
    if callback.is_ok() {
        return Err(PanicScenarioError::MissingCallbackPanic);
    }
    let terminal = owner
        .poll_terminal()?
        .ok_or(PanicScenarioError::MissingTerminal)?;
    assert_eq!(terminal.outcome, TerminalOutcome::ExecutorUnwound);
    assert_eq!(drops.load(Ordering::Acquire), 1);
    let metrics = runtime.metrics();
    assert_eq!(metrics.available, metrics.capacity);
    assert_eq!(metrics.checked_out, 0);
    assert_eq!(metrics.reserved_bytes, 0);
    assert_eq!(metrics.terminal_occupied, 0);
    Ok(())
}

#[test]
fn typed_callback_failure_is_retained_without_panic() -> Result<(), PanicScenarioError> {
    let drops = std::sync::Arc::new(core::sync::atomic::AtomicUsize::new(0));
    let mut runtime = ObservedRuntime::<u8, PanicWork, CallbackFailure>::new(1, budget(1)?)?;
    let (admission, mut owner) = runtime.split();
    admission.admit(
        1,
        PanicWork {
            bytes: RetainedBytes::from(1),
            drops: std::sync::Arc::clone(&drops),
            panic_on_drop: false,
        },
    )?;
    assert_eq!(
        owner.execute_next(&1, |_| Err(CallbackFailure { code: 7 }))?,
        crate::OwnerProgress::Terminalized
    );
    let terminal = owner
        .poll_terminal()?
        .ok_or(PanicScenarioError::MissingTerminal)?;
    assert_eq!(
        terminal.outcome,
        TerminalOutcome::Failed {
            failure: CallbackFailure { code: 7 }
        }
    );
    assert_eq!(drops.load(Ordering::Acquire), 1);
    Ok(())
}

#[test]
fn wrong_readiness_lanes_restore_payload_and_exact_terminal() -> Result<(), TestError> {
    let mut runtime = ObservedRuntime::<u8, Work>::new(1, budget(1)?)?;
    let (admission, mut owner) = runtime.split();
    let handle = admission.admit(
        1,
        Work {
            bytes: RetainedBytes::from(1),
        },
    )?;

    admission.fabric.terminal_ready.publish(handle.index());
    assert_eq!(
        owner.poll_terminal(),
        Err(crate::OwnerFault::TerminalReadyContainedWork { handle })
    );
    assert_eq!(
        owner.execute_next(&1, |_| Ok(())),
        Ok(crate::OwnerProgress::Terminalized)
    );

    admission.fabric.ready.publish(handle.index());
    assert_eq!(
        owner.execute_next(&1, |_| Ok(())),
        Err(crate::OwnerFault::WorkReadyContainedTerminal { handle })
    );
    let terminal = owner.poll_terminal()?.ok_or(TestError::MissingTerminal)?;
    assert_eq!(terminal.handle, handle);
    assert_eq!(terminal.generation, 1);
    assert_eq!(terminal.outcome, TerminalOutcome::Completed);
    assert_eq!(
        runtime.metrics(),
        crate::RuntimeMetrics {
            capacity: 1,
            active_capacity: 1,
            available: 1,
            checked_out: 0,
            reserved_bytes: 0,
            retired_work_slots: 0,
            terminal_occupied: 0,
        }
    );
    Ok(())
}

#[test]
fn destructor_panic_after_normal_callback_keeps_the_committed_terminal()
-> Result<(), PanicScenarioError> {
    let drops = std::sync::Arc::new(core::sync::atomic::AtomicUsize::new(0));
    let mut runtime = ObservedRuntime::<u8, PanicWork>::new(1, budget(1)?)?;
    let (admission, mut owner) = runtime.split();
    admission.admit(
        1,
        PanicWork {
            bytes: RetainedBytes::from(1),
            drops: std::sync::Arc::clone(&drops),
            panic_on_drop: true,
        },
    )?;
    let dropped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        owner.execute_next(&1, |_| Ok(()))
    }));
    if dropped.is_ok() {
        return Err(PanicScenarioError::MissingDropPanic);
    }
    let terminal = owner
        .poll_terminal()?
        .ok_or(PanicScenarioError::MissingTerminal)?;
    assert_eq!(terminal.outcome, TerminalOutcome::Completed);
    assert_eq!(drops.load(Ordering::Acquire), 1);
    let metrics = runtime.metrics();
    assert_eq!(metrics.available, metrics.capacity);
    assert_eq!(metrics.checked_out, 0);
    assert_eq!(metrics.reserved_bytes, 0);
    assert_eq!(metrics.terminal_occupied, 0);
    Ok(())
}
