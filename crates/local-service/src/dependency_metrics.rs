//! Owner-scoped dependency measurements.
use backend_client::dependency_metrics::{BOUNDS_SECONDS, Snapshot};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Clone, Debug, Default)]
pub(crate) struct Metrics(Arc<Mutex<Snapshot>>);

impl Metrics {
    pub(crate) fn snapshot(&self) -> Snapshot {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn observe<T, E>(
        &self,
        operation: usize,
        f: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        let start = Instant::now();
        let result = f();
        let elapsed = start.elapsed();
        let nanos = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX);
        let mut snapshot = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let op = match operation {
            0 => &mut snapshot.recover,
            1 => &mut snapshot.append,
            _ => &mut snapshot.replace,
        };
        let outcome = if result.is_ok() {
            &mut op.success
        } else {
            &mut op.error
        };
        outcome.count = outcome.count.saturating_add(1);
        outcome.duration_nanos = outcome.duration_nanos.saturating_add(nanos);
        for (index, bound) in BOUNDS_SECONDS.iter().enumerate() {
            if elapsed.as_secs_f64() <= *bound {
                outcome.buckets[index] = outcome.buckets[index].saturating_add(1);
            }
        }
        outcome.buckets[7] = outcome.buckets[7].saturating_add(1);
        result
    }
}
