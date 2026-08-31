use std::{
    process::Command,
    time::{Duration, Instant},
};

use allocation_counter::{AllocationInfo, measure};

use crate::{
    BenchmarkError,
    model::{Allocations, CacheMode, Metric, MetricUnavailable, Stage, StageSample},
};

/// Logical I/O facts produced by one measured public API operation.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StageWork {
    /// Representative input items consumed.
    pub(crate) input_items: usize,
    /// Result items produced.
    pub(crate) output_items: usize,
    /// Logical input bytes read.
    pub(crate) bytes_read: u64,
    /// Logical output bytes written.
    pub(crate) bytes_written: u64,
    /// Durable regular-file bytes after the operation.
    pub(crate) durable_bytes: u64,
}

/// Runs exactly one public operation behind the established allocator counter.
pub(crate) fn stage(
    stage: Stage,
    cache_mode: CacheMode,
    sample: usize,
    operation: impl FnOnce() -> Result<StageWork, BenchmarkError>,
) -> Result<StageSample, BenchmarkError> {
    let start = Instant::now();
    let mut result = None;
    let allocation_info = measure(|| result = Some(operation()));
    let elapsed = start.elapsed();
    let work = result.ok_or(BenchmarkError::MeasurementDidNotRun)?;
    let work = work?;
    Ok(StageSample {
        stage,
        cache_mode,
        sample,
        input_items: work.input_items,
        output_items: work.output_items,
        bytes_read: work.bytes_read,
        bytes_written: work.bytes_written,
        durable_bytes: work.durable_bytes,
        wall_time_ns: elapsed.as_nanos(),
        cpu_time_ns: Metric::Unavailable {
            reason: MetricUnavailable::ExternalProcessAccountingRequired,
        },
        peak_rss_bytes: Metric::Unavailable {
            reason: MetricUnavailable::ExternalPeakRssWrapperRequired,
        },
        rss_after_bytes: current_rss_bytes(),
        allocations: allocations(allocation_info),
        input_throughput_bytes_per_second: throughput(work.bytes_read, elapsed),
    })
}

const fn allocations(info: AllocationInfo) -> Allocations {
    Allocations {
        count_total: info.count_total,
        count_current: info.count_current,
        count_max: info.count_max,
        bytes_total: info.bytes_total,
        bytes_current: info.bytes_current,
        bytes_max: info.bytes_max,
    }
}

#[allow(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "the result schema uses f64 for a conventional rate; bounded local corpus byte counts remain exactly representable"
)]
fn throughput(bytes: u64, elapsed: Duration) -> f64 {
    let seconds = elapsed.as_secs_f64();
    if seconds == 0.0 {
        0.0
    } else {
        bytes as f64 / seconds
    }
}

fn current_rss_bytes() -> Metric {
    let process = std::process::id().to_string();
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &process])
        .output();
    let Ok(output) = output else {
        return Metric::Unavailable {
            reason: MetricUnavailable::ProcessProbeUnavailable,
        };
    };
    if !output.status.success() {
        return Metric::Unavailable {
            reason: MetricUnavailable::ProcessProbeUnavailable,
        };
    }
    let Ok(text) = core::str::from_utf8(&output.stdout) else {
        return Metric::Unavailable {
            reason: MetricUnavailable::ProcessProbeUnavailable,
        };
    };
    let Some(kibibytes) = text.split_whitespace().next() else {
        return Metric::Unavailable {
            reason: MetricUnavailable::ProcessProbeUnavailable,
        };
    };
    let Ok(kibibytes) = kibibytes.parse::<u64>() else {
        return Metric::Unavailable {
            reason: MetricUnavailable::ProcessProbeUnavailable,
        };
    };
    let Some(bytes) = kibibytes.checked_mul(1024) else {
        return Metric::Unavailable {
            reason: MetricUnavailable::ProcessProbeUnavailable,
        };
    };
    Metric::Measured { value: bytes }
}
