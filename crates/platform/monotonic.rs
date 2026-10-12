//! A Linux native-clock bracket for opt-in external frame observation.
//! No wall clock, renderer, timer, thread or process is created here.

use crate::FileIdentity;
use std::fs::File;
use std::io;
use std::time::Instant;

/// One Rust Instant lies between two reads of the same Linux CLOCK_MONOTONIC
/// used by external native observers. The retained namespace handle pins its
/// identity; observers must match the app's current namespace to their own.
pub struct EpochAnchor {
    epoch: Instant,
    interval_ns: [u64; 2],
    namespace: FileIdentity,
    _namespace: File,
}

impl EpochAnchor {
    /// Capture once when opt-in tracing starts; hold this until tracing ends.
    ///
    /// # Errors
    /// Refuses missing namespace identity or invalid/overflowing clock values.
    pub fn capture() -> io::Result<Self> {
        let namespace = File::open("/proc/self/ns/time")?;
        let identity = FileIdentity::of_file(&namespace)?;
        let before = nanos(rustix::time::clock_gettime(rustix::time::ClockId::Monotonic))?;
        let epoch = Instant::now();
        let after = nanos(rustix::time::clock_gettime(rustix::time::ClockId::Monotonic))?;
        if before > after {
            return Err(io::Error::other("native monotonic clock moved backwards"));
        }
        Ok(Self { epoch, interval_ns: [before, after], namespace: identity, _namespace: namespace })
    }

    /// Returns the `Instant` captured between the two native clock reads.
    #[must_use]
    pub const fn epoch(&self) -> Instant { self.epoch }

    /// Returns the lower and upper native nanosecond bounds around `epoch`.
    #[must_use]
    pub const fn interval_ns(&self) -> [u64; 2] { self.interval_ns }

    /// Returns the identity of the time namespace held for this anchor.
    #[must_use]
    pub const fn namespace(&self) -> FileIdentity { self.namespace }
}

fn nanos(time: rustix::time::Timespec) -> io::Result<u64> {
    let seconds = u64::try_from(time.tv_sec).map_err(|_| io::Error::other("negative monotonic seconds"))?;
    let fraction = u64::try_from(time.tv_nsec).map_err(|_| io::Error::other("negative monotonic fraction"))?;
    if fraction >= 1_000_000_000 {
        return Err(io::Error::other("invalid monotonic fraction"));
    }
    seconds.checked_mul(1_000_000_000).and_then(|value| value.checked_add(fraction))
        .ok_or_else(|| io::Error::other("native monotonic nanoseconds overflow"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_anchor_pins_the_current_namespace_and_brackets_later_monotonic_reads() {
        let anchor = EpochAnchor::capture().expect("Linux native clock and namespace");
        let namespace = File::open("/proc/self/ns/time").expect("current time namespace");
        assert_eq!(anchor.namespace(), FileIdentity::of_file(&namespace).expect("namespace identity"));
        let interval = anchor.interval_ns();
        assert!(interval[0] <= interval[1]);
        let later = nanos(rustix::time::clock_gettime(rustix::time::ClockId::Monotonic)).expect("native clock");
        assert!(interval[1] <= later);
        assert!(anchor.epoch() <= Instant::now());
    }

    #[test]
    fn invalid_clock_values_never_become_an_external_anchor() {
        for (seconds, fraction) in [(-1, 0), (0, -1), (0, 1_000_000_000), (i64::MAX, 0)] {
            assert!(nanos(rustix::time::Timespec { tv_sec: seconds, tv_nsec: fraction }).is_err());
        }
        assert_eq!(nanos(rustix::time::Timespec { tv_sec: 3, tv_nsec: 7 }).expect("valid clock"), 3_000_000_007);
    }
}
