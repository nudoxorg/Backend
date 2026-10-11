//! Per-call work accounting for cancellable union construction.
//!
//! A `TypeInterner` is shared across checker workers, so cancellation cannot be
//! stored on it. Budgeted callers instead lend this small meter through the
//! complete union-normalization call. Its polling cadence matches
//! `ExecutionWorkMeter`; unbudgeted construction does not allocate a meter.

use tsz_common::{ExecutionCheckpoint, ProjectExecutionStop};

pub(crate) struct UnionExecutionWork<'checkpoint> {
    checkpoint: &'checkpoint dyn ExecutionCheckpoint,
    pending_work_units: u64,
}

impl<'checkpoint> UnionExecutionWork<'checkpoint> {
    pub(crate) fn new(
        checkpoint: &'checkpoint dyn ExecutionCheckpoint,
    ) -> Result<Self, ProjectExecutionStop> {
        checkpoint.checkpoint(0)?;
        Ok(Self {
            checkpoint,
            pending_work_units: 0,
        })
    }

    pub(crate) fn tick(&mut self, work_units: u64) -> Result<(), ProjectExecutionStop> {
        self.pending_work_units = self.pending_work_units.saturating_add(work_units);
        if self.pending_work_units >= 64 {
            self.flush()?;
        }
        Ok(())
    }

    /// Poll before a normalization result, memo entry, or complexity signal is
    /// published. This also flushes any work accumulated since the last poll.
    pub(crate) fn flush(&mut self) -> Result<(), ProjectExecutionStop> {
        let charged = std::mem::take(&mut self.pending_work_units);
        self.checkpoint.checkpoint(charged)
    }
}

pub(crate) fn tick_union_execution_work(
    work: &mut Option<&mut UnionExecutionWork<'_>>,
    work_units: u64,
) -> Result<(), ProjectExecutionStop> {
    if let Some(work) = work.as_deref_mut() {
        work.tick(work_units)?;
    }
    Ok(())
}

pub(crate) fn flush_union_execution_work(
    work: &mut Option<&mut UnionExecutionWork<'_>>,
) -> Result<(), ProjectExecutionStop> {
    if let Some(work) = work.as_deref_mut() {
        work.flush()?;
    }
    Ok(())
}
