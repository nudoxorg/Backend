//! Compact publication of permit-addressed payload slots.
use core::num::NonZeroU64;
#[cfg(not(all(test, feature = "loom-model")))]
use core::sync::atomic::{AtomicU64, Ordering};
#[cfg(all(test, feature = "loom-model"))]
use loom::sync::atomic::{AtomicU64, Ordering};

use crate::slot::SlotIndex;

/// One ready bit per physical work coordinate. It never owns a `Work` value.
pub(crate) struct ReadyBitmap {
    core: ReadyCore,
}

impl ReadyBitmap {
    #[allow(
        clippy::missing_const_for_fn,
        reason = "the identical Loom-selected constructor is not const"
    )]
    pub(crate) fn new() -> Self {
        Self {
            core: ReadyCore::new(),
        }
    }

    /// Publishes a fully initialized payload. Release pairs with `take` before it reads the cell.
    pub(crate) fn publish(&self, index: SlotIndex) {
        self.core.publish(index);
    }

    /// Claims exactly one ready coordinate. The successful acquire synchronizes payload access.
    pub(crate) fn take(&self) -> Option<SlotIndex> {
        self.core.take()
    }

    pub(crate) fn count(&self) -> usize {
        self.core.count()
    }
}

pub(crate) struct ReadyCore {
    bits: AtomicU64,
}

impl ReadyCore {
    #[allow(
        clippy::missing_const_for_fn,
        reason = "the identical Loom-selected constructor is not const"
    )]
    pub(crate) fn new() -> Self {
        Self {
            bits: AtomicU64::new(0),
        }
    }

    pub(crate) fn publish(&self, index: SlotIndex) {
        self.bits.fetch_or(index.mask(), Ordering::Release);
    }

    #[allow(
        clippy::as_conversions,
        reason = "a nonzero bitmap's trailing-zero count is always a valid word coordinate"
    )]
    pub(crate) fn take(&self) -> Option<SlotIndex> {
        let mut observed = self.bits.load(Ordering::Acquire);
        loop {
            if observed == 0 {
                return None;
            }
            let bits = NonZeroU64::new(observed)?;
            let index = SlotIndex::from_ready_word(bits);
            let claimed = observed & !index.mask();
            match self.bits.compare_exchange_weak(
                observed,
                claimed,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(index),
                Err(actual) => observed = actual,
            }
        }
    }

    #[allow(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "a u64 bitmap has at most 64 set bits, which always fit in usize"
    )]
    pub(crate) fn count(&self) -> usize {
        self.bits.load(Ordering::Acquire).count_ones() as usize
    }
}

#[cfg(all(test, feature = "loom-model"))]
mod tests {
    use loom::{sync::Arc, thread};
    use thiserror::Error;

    use crate::slot::SlotIndex;

    use super::ReadyCore;

    #[test]
    fn loom_publish_then_take_returns_the_same_physical_coordinate() {
        loom::model(|| crate::test_report::assert_loom(loom_transition));
    }

    fn loom_transition() -> Result<(), ReadyTestError> {
        let ready = Arc::new(ReadyCore::new());
        let producer_ready = Arc::clone(&ready);
        let producer = thread::spawn(move || {
            producer_ready.publish(SlotIndex::from_ready_word(core::num::NonZeroU64::MIN));
        });
        crate::test_report::join(producer.join())?;
        let first = ready.take();
        if first.map(SlotIndex::array_index) != Some(0) {
            return Err(ReadyTestError::FirstTake {
                observed: first.map(SlotIndex::array_index),
            });
        }
        let second = ready.take();
        if second.is_some() {
            return Err(ReadyTestError::SecondTake {
                observed: second.map(SlotIndex::array_index),
            });
        }
        Ok(())
    }

    #[derive(Debug, Error)]
    enum ReadyTestError {
        #[error("ready producer panicked")]
        Join(#[from] crate::test_report::ThreadPanic),
        #[error("first ready dequeue expected coordinate 3 but observed {observed:?}")]
        FirstTake { observed: Option<usize> },
        #[error("ready bitmap retained an already-consumed coordinate {observed:?}")]
        SecondTake { observed: Option<usize> },
    }
}
