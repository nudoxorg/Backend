//! Bounded dedicated-thread trace and log exporter construction.

use core::{num::NonZeroUsize, time::Duration};

use opentelemetry_sdk::{
    logs::{
        BatchConfigBuilder as LogBatchConfigBuilder, BatchLogProcessor, LogExporter,
        SdkLoggerProvider,
    },
    trace::{
        BatchConfigBuilder as SpanBatchConfigBuilder, BatchSpanProcessor, SdkTracerProvider,
        SpanExporter,
    },
};
use thiserror::Error;

/// Validated finite queue/batch limits for production background exporters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BatchLimits {
    max_queue: NonZeroUsize,
    max_export_batch: NonZeroUsize,
    scheduled_delay: Duration,
}

/// Invalid production batch configuration.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum BatchLimitsError {
    /// A queue with no representable record cannot provide useful export.
    #[error("batch queue capacity must be non-zero")]
    ZeroQueue,
    /// An empty export batch cannot make progress.
    #[error("maximum export batch must be non-zero")]
    ZeroBatch,
    /// One export batch cannot exceed its retaining queue.
    #[error("maximum export batch {batch} exceeds queue capacity {queue}")]
    BatchExceedsQueue {
        /// Requested maximum export batch.
        batch: usize,
        /// Retaining queue capacity.
        queue: usize,
    },
}

impl BatchLimits {
    /// Validates the bounded nonblocking queue relationship once at startup.
    ///
    /// # Errors
    ///
    /// Returns the exact zero or batch/queue mismatch.
    pub const fn new(
        max_queue: usize,
        max_export_batch: usize,
        scheduled_delay: Duration,
    ) -> Result<Self, BatchLimitsError> {
        let Some(max_queue) = NonZeroUsize::new(max_queue) else {
            return Err(BatchLimitsError::ZeroQueue);
        };
        let Some(max_export_batch) = NonZeroUsize::new(max_export_batch) else {
            return Err(BatchLimitsError::ZeroBatch);
        };
        if max_export_batch.get() > max_queue.get() {
            return Err(BatchLimitsError::BatchExceedsQueue {
                batch: max_export_batch.get(),
                queue: max_queue.get(),
            });
        }
        Ok(Self {
            max_queue,
            max_export_batch,
            scheduled_delay,
        })
    }
}

/// Builds a dedicated-thread span processor with a bounded drop-on-full queue.
pub fn batch_provider<Exporter>(exporter: Exporter, limits: BatchLimits) -> SdkTracerProvider
where
    Exporter: SpanExporter + 'static,
{
    let config = SpanBatchConfigBuilder::default()
        .with_max_queue_size(limits.max_queue.get())
        .with_max_export_batch_size(limits.max_export_batch.get())
        .with_scheduled_delay(limits.scheduled_delay)
        .build();
    let processor = BatchSpanProcessor::builder(exporter)
        .with_batch_config(config)
        .build();
    SdkTracerProvider::builder()
        .with_span_processor(processor)
        .build()
}

/// Builds a dedicated-thread log processor with a bounded drop-on-full queue.
pub fn batch_logger_provider<Exporter>(exporter: Exporter, limits: BatchLimits) -> SdkLoggerProvider
where
    Exporter: LogExporter + 'static,
{
    let config = LogBatchConfigBuilder::default()
        .with_max_queue_size(limits.max_queue.get())
        .with_max_export_batch_size(limits.max_export_batch.get())
        .with_scheduled_delay(limits.scheduled_delay)
        .build();
    let processor = BatchLogProcessor::builder(exporter)
        .with_batch_config(config)
        .build();
    SdkLoggerProvider::builder()
        .with_log_processor(processor)
        .build()
}
