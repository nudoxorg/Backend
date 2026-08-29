use std::sync::{
    Arc, Condvar, Mutex,
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

#[derive(Clone, Debug)]
struct BlockingGate {
    state: Arc<(Mutex<GateState>, Condvar)>,
}

#[derive(Debug, Default)]
struct GateState {
    started: bool,
    released: bool,
    exported: usize,
    largest_batch: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ExportedBatches {
    count: usize,
    largest: usize,
}

#[derive(Debug)]
struct ProducerOutcome {
    received: Result<usize, AdapterTestError>,
    released: Result<(), AdapterTestError>,
    joined: Result<Result<(), AdapterTestError>, AdapterTestError>,
}

impl ProducerOutcome {
    // Coordination has deterministic source priority: receive, release, join, producer send.
    fn into_result(self) -> Result<usize, AdapterTestError> {
        let completed = self.received?;
        self.released?;
        self.joined??;
        Ok(completed)
    }
}

impl BlockingGate {
    fn new() -> Self {
        Self {
            state: Arc::new((Mutex::new(GateState::default()), Condvar::new())),
        }
    }

    fn block_export(&self, batch: usize) -> Result<(), AdapterTestError> {
        let (lock, wake) = &*self.state;
        let mut state = lock.lock().map_err(|_| AdapterTestError::GatePoisoned)?;
        state.started = true;
        state.largest_batch = state.largest_batch.max(batch);
        wake.notify_one();
        let (state, waited) = wake
            .wait_timeout_while(state, TEST_TIMEOUT, |state| !state.released)
            .map_err(|_| AdapterTestError::GatePoisoned)?;
        if waited.timed_out() && !state.released {
            return Err(AdapterTestError::GateTimedOut);
        }
        drop(state);
        let mut state = lock.lock().map_err(|_| AdapterTestError::GatePoisoned)?;
        state.exported += batch;
        Ok(())
    }

    fn wait_started(&self) -> Result<(), AdapterTestError> {
        let (lock, wake) = &*self.state;
        let state = lock.lock().map_err(|_| AdapterTestError::GatePoisoned)?;
        let (state, waited) = wake
            .wait_timeout_while(state, TEST_TIMEOUT, |state| !state.started)
            .map_err(|_| AdapterTestError::GatePoisoned)?;
        if waited.timed_out() && !state.started {
            return Err(AdapterTestError::GateTimedOut);
        }
        Ok(())
    }

    fn release(&self) -> Result<(), AdapterTestError> {
        let (lock, wake) = &*self.state;
        let mut state = lock.lock().map_err(|_| AdapterTestError::GatePoisoned)?;
        state.released = true;
        wake.notify_one();
        Ok(())
    }

    fn exported(&self) -> Result<ExportedBatches, AdapterTestError> {
        let (lock, _) = &*self.state;
        let state = lock.lock().map_err(|_| AdapterTestError::GatePoisoned)?;
        Ok(ExportedBatches {
            count: state.exported,
            largest: state.largest_batch,
        })
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
    gate: &BlockingGate,
    produce: Produce,
) -> Result<usize, AdapterTestError>
where
    Produce: FnOnce() -> usize + Send,
{
    std::thread::scope(|scope| {
        let (complete, observed) = sync_channel(1);
        let producer = scope.spawn(move || {
            complete
                .send(produce())
                .map_err(|_| AdapterTestError::ProducerDisconnected)
        });
        let received = observed
            .recv_timeout(TEST_TIMEOUT)
            .map_err(|error| match error {
                RecvTimeoutError::Timeout => AdapterTestError::ProducerTimedOut,
                RecvTimeoutError::Disconnected => AdapterTestError::ProducerDisconnected,
            });
        let released = gate.release();
        let joined = producer
            .join()
            .map_err(|_| AdapterTestError::ProducerPanicked);
        ProducerOutcome {
            received,
            released,
            joined,
        }
        .into_result()
    })
}

#[test]
fn producer_outcome_retains_coexisting_failures_before_prioritizing() {
    let outcome = ProducerOutcome {
        received: Err(AdapterTestError::ProducerTimedOut),
        released: Err(AdapterTestError::GatePoisoned),
        joined: Err(AdapterTestError::ProducerPanicked),
    };
    assert!(matches!(
        &outcome.received,
        Err(AdapterTestError::ProducerTimedOut)
    ));
    assert!(matches!(
        &outcome.released,
        Err(AdapterTestError::GatePoisoned)
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
    let gate = BlockingGate::new();
    let provider = batch_provider(
        BlockingSpanExporter(gate.clone()),
        BatchLimits::new(1, 1, core::time::Duration::from_hours(1))?,
    );
    drop(provider.tracer("queue-test").start("first"));
    gate.wait_started()?;
    let tracer = provider.tracer("queue-test");
    let completed = producer_completes_before_release(&gate, move || {
        (0..FOLLOW_UP_ATTEMPTS)
            .map(|_| {
                drop(tracer.start("overload"));
            })
            .count()
    })?;
    assert_eq!(completed, FOLLOW_UP_ATTEMPTS);
    provider.force_flush()?;
    assert_eq!(
        gate.exported()?,
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
    let gate = BlockingGate::new();
    let trace_provider = batch_provider(
        opentelemetry_sdk::trace::InMemorySpanExporterBuilder::new().build(),
        BatchLimits::new(1, 1, core::time::Duration::from_hours(1))?,
    );
    let logger_provider = batch_logger_provider(
        BlockingLogExporter(gate.clone()),
        BatchLimits::new(1, 1, core::time::Duration::from_hours(1))?,
    );
    let subscriber = dispatch(&trace_provider, &logger_provider, nudox_interest());
    tracing::dispatcher::with_default(&subscriber, || {
        tracing::info!(target: "nudox.overload", sequence = 0_u8, "queue overload");
    });
    gate.wait_started()?;
    let completed = producer_completes_before_release(&gate, move || {
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
        gate.exported()?,
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
