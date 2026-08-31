use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc::{RecvTimeoutError, sync_channel},
};

use opentelemetry::trace::{Tracer as _, TracerProvider as _};
use opentelemetry_sdk::{
    error::OTelSdkError,
    logs::{LogBatch, LogExporter},
    trace::{SpanData, SpanExporter},
};

use nudox_observability_adapter::{
    BatchLimits, BatchLimitsError, batch_logger_provider, batch_provider, dispatch,
};

use super::support::{AdapterTestError, nudox_interest};

const OVERLOAD_ATTEMPTS: usize = 12;
const FOLLOW_UP_ATTEMPTS: usize = OVERLOAD_ATTEMPTS - 1;
const TEST_TIMEOUT: core::time::Duration = core::time::Duration::from_secs(1);

#[derive(Debug)]
struct BlockingGate {
    state: Arc<GateState>,
    started: std::sync::mpsc::SyncSender<std::thread::Thread>,
}

#[derive(Debug, Default)]
struct GateState {
    waiting: AtomicBool,
    released: AtomicBool,
    exported: AtomicUsize,
    largest_batch: AtomicUsize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ExportedBatches {
    count: usize,
    largest: usize,
}

#[derive(Debug)]
struct ProducerOutcome {
    received: Result<usize, AdapterTestError>,
    joined: Result<Result<(), AdapterTestError>, AdapterTestError>,
}

impl ProducerOutcome {
    // Coordination has deterministic source priority: receive, join, producer send.
    fn into_result(self) -> Result<usize, AdapterTestError> {
        let completed = self.received?;
        self.joined??;
        Ok(completed)
    }
}

#[derive(Debug)]
struct GateController {
    state: Arc<GateState>,
    started: std::sync::mpsc::Receiver<std::thread::Thread>,
}

#[derive(Debug)]
struct BlockedExport {
    state: Arc<GateState>,
    worker: std::thread::Thread,
}

#[derive(Debug)]
struct ReleasedGate(Arc<GateState>);

fn blocking_gate() -> (BlockingGate, GateController) {
    let state = Arc::new(GateState::default());
    let (started, observed) = sync_channel(1);
    (
        BlockingGate {
            state: Arc::clone(&state),
            started,
        },
        GateController {
            state,
            started: observed,
        },
    )
}

impl BlockingGate {
    fn block_export(&self, batch: usize) -> Result<(), AdapterTestError> {
        self.state.largest_batch.fetch_max(batch, Ordering::Relaxed);
        if self.state.released.load(Ordering::Acquire) {
            self.state.exported.fetch_add(batch, Ordering::Release);
            return Ok(());
        }
        if self
            .state
            .waiting
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(AdapterTestError::ConcurrentBlockedExport);
        }
        if self.started.send(std::thread::current()).is_err() {
            return Err(AdapterTestError::GateControllerDropped);
        }

        let started = std::time::Instant::now();
        while !self.state.released.load(Ordering::Acquire) {
            let Some(remaining) = TEST_TIMEOUT.checked_sub(started.elapsed()) else {
                return Err(AdapterTestError::GateTimedOut);
            };
            std::thread::park_timeout(remaining);
        }
        self.state.exported.fetch_add(batch, Ordering::Release);
        Ok(())
    }
}

impl GateController {
    fn wait_started(self) -> Result<BlockedExport, AdapterTestError> {
        match self.started.recv_timeout(TEST_TIMEOUT) {
            Ok(worker) => Ok(BlockedExport {
                state: self.state,
                worker,
            }),
            Err(RecvTimeoutError::Timeout) => Err(AdapterTestError::GateTimedOut),
            Err(RecvTimeoutError::Disconnected) => Err(AdapterTestError::GateControllerDropped),
        }
    }
}

impl BlockedExport {
    fn release(self) -> ReleasedGate {
        self.state.released.store(true, Ordering::Release);
        self.worker.unpark();
        ReleasedGate(self.state)
    }
}

impl ReleasedGate {
    fn exported(&self) -> ExportedBatches {
        ExportedBatches {
            count: self.0.exported.load(Ordering::Acquire),
            largest: self.0.largest_batch.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug)]
struct BlockingSpanExporter(BlockingGate);

impl SpanExporter for BlockingSpanExporter {
    fn export(
        &self,
        batch: Vec<SpanData>,
    ) -> impl std::future::Future<Output = Result<(), OTelSdkError>> + Send {
        let result = self.0.block_export(batch.len());
        async move { result.map_err(AdapterTestError::into_sdk_error) }
    }
}

#[derive(Debug)]
struct BlockingLogExporter(BlockingGate);

impl LogExporter for BlockingLogExporter {
    fn export(
        &self,
        batch: LogBatch<'_>,
    ) -> impl std::future::Future<Output = Result<(), OTelSdkError>> + Send {
        let result = self.0.block_export(batch.iter().count());
        async move { result.map_err(AdapterTestError::into_sdk_error) }
    }
}

fn producer_completes_before_release<Produce>(
    blocked: BlockedExport,
    produce: Produce,
) -> Result<(usize, ReleasedGate), AdapterTestError>
where
    Produce: FnOnce() -> usize + Send,
{
    std::thread::scope(|scope| {
        let (complete, observed) = sync_channel(1);
        let producer = scope.spawn(move || match complete.send(produce()) {
            Ok(()) => Ok(()),
            Err(disconnected) => Err(AdapterTestError::ProducerDisconnected {
                completed: disconnected.0,
            }),
        });
        let received = observed
            .recv_timeout(TEST_TIMEOUT)
            .map_err(|error| match error {
                RecvTimeoutError::Timeout => AdapterTestError::ProducerTimedOut,
                RecvTimeoutError::Disconnected => AdapterTestError::ProducerCompletionLost,
            });
        let released = blocked.release();
        let joined = match producer.join() {
            Ok(produced) => Ok(produced),
            Err(_panic) => Err(AdapterTestError::ProducerPanicked),
        };
        ProducerOutcome { received, joined }
            .into_result()
            .map(|completed| (completed, released))
    })
}

#[test]
fn producer_outcome_retains_coexisting_failures_before_prioritizing() {
    let outcome = ProducerOutcome {
        received: Err(AdapterTestError::ProducerTimedOut),
        joined: Err(AdapterTestError::ProducerPanicked),
    };
    assert!(matches!(
        &outcome.received,
        Err(AdapterTestError::ProducerTimedOut)
    ));
    assert!(matches!(
        &outcome.joined,
        Err(AdapterTestError::ProducerPanicked)
    ));
    assert!(matches!(
        outcome.into_result(),
        Err(AdapterTestError::ProducerTimedOut)
    ));
}

#[test]
fn bounded_span_queue_drops_without_blocking_the_completed_producer() -> Result<(), AdapterTestError>
{
    let (gate, controller) = blocking_gate();
    let provider = batch_provider(
        BlockingSpanExporter(gate),
        BatchLimits::new(1, 1, core::time::Duration::from_hours(1))?,
    );
    drop(provider.tracer("queue-test").start("first"));
    let blocked = controller.wait_started()?;
    let tracer = provider.tracer("queue-test");
    let (completed, released) = producer_completes_before_release(blocked, move || {
        (0..FOLLOW_UP_ATTEMPTS)
            .map(|_| {
                drop(tracer.start("overload"));
            })
            .count()
    })?;
    assert_eq!(completed, FOLLOW_UP_ATTEMPTS);
    provider.force_flush()?;
    assert_eq!(
        released.exported(),
        ExportedBatches {
            count: 2,
            largest: 1
        }
    );
    provider.shutdown()?;
    Ok(())
}

#[test]
fn bounded_log_queue_drops_without_blocking_the_completed_producer() -> Result<(), AdapterTestError>
{
    let (gate, controller) = blocking_gate();
    let trace_provider = batch_provider(
        opentelemetry_sdk::trace::InMemorySpanExporterBuilder::new().build(),
        BatchLimits::new(1, 1, core::time::Duration::from_hours(1))?,
    );
    let logger_provider = batch_logger_provider(
        BlockingLogExporter(gate),
        BatchLimits::new(1, 1, core::time::Duration::from_hours(1))?,
    );
    let subscriber = dispatch(&trace_provider, &logger_provider, nudox_interest());
    tracing::dispatcher::with_default(&subscriber, || {
        tracing::info!(target: "nudox.overload", sequence = 0_u8, "queue overload");
    });
    let blocked = controller.wait_started()?;
    let (completed, released) = producer_completes_before_release(blocked, move || {
        tracing::dispatcher::with_default(&subscriber, || {
            (1..=FOLLOW_UP_ATTEMPTS)
                .map(|sequence| {
                    tracing::info!(target: "nudox.overload", sequence, "queue overload");
                })
                .count()
        })
    })?;
    assert_eq!(completed, FOLLOW_UP_ATTEMPTS);
    logger_provider.force_flush()?;
    assert_eq!(
        released.exported(),
        ExportedBatches {
            count: 2,
            largest: 1
        }
    );
    trace_provider.shutdown()?;
    logger_provider.shutdown()?;
    Ok(())
}

#[test]
fn batch_limits_retain_exact_configuration_rejections() {
    let delay = core::time::Duration::from_secs(1);
    assert_eq!(
        BatchLimits::new(0, 1, delay),
        Err(BatchLimitsError::ZeroQueue)
    );
    assert_eq!(
        BatchLimits::new(1, 0, delay),
        Err(BatchLimitsError::ZeroBatch)
    );
    assert_eq!(
        BatchLimits::new(2, 3, delay),
        Err(BatchLimitsError::BatchExceedsQueue { batch: 3, queue: 2 })
    );
}
