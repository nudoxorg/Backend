//! Bounded scalar snapshots for publication facts.
//!
//! The publication owner is the only writer. Readers may race that owner, so
//! the version cell brackets relaxed payload stores with release/acquire
//! publication. Every payload cell is atomic: an interrupted reader can
//! observe only an explicitly rejected mixed version, never a Rust data race.

#[cfg(all(test, feature = "loom-model"))]
use loom::sync::atomic::{AtomicU64, Ordering};
#[cfg(not(all(test, feature = "loom-model")))]
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(not(all(test, feature = "loom-model")))]
const READ_ATTEMPTS: u8 = 64;
#[cfg(all(test, feature = "loom-model"))]
const READ_ATTEMPTS: u8 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SnapshotWriteError {
    ConcurrentWriter { observed_version: u64 },
    VersionExhausted { observed_version: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SnapshotReadError {
    pub(super) attempts: u8,
    pub(super) observed_version: u64,
}

pub(super) struct AtomicSnapshot<const WORDS: usize> {
    version: AtomicU64,
    words: [AtomicU64; WORDS],
}

impl<const WORDS: usize> AtomicSnapshot<WORDS> {
    pub(super) fn empty() -> Self {
        Self {
            version: AtomicU64::new(0),
            words: core::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    pub(super) fn replace(&self, words: [u64; WORDS]) -> Result<(), SnapshotWriteError> {
        let observed_version = self.version.load(Ordering::Acquire);
        if observed_version & 1 != 0 {
            return Err(SnapshotWriteError::ConcurrentWriter { observed_version });
        }
        let writing_version = observed_version
            .checked_add(1)
            .ok_or(SnapshotWriteError::VersionExhausted { observed_version })?;
        let published_version = writing_version
            .checked_add(1)
            .ok_or(SnapshotWriteError::VersionExhausted { observed_version })?;
        self.version
            .compare_exchange(
                observed_version,
                writing_version,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|observed_version| SnapshotWriteError::ConcurrentWriter {
                observed_version,
            })?;
        for (target, value) in self.words.iter().zip(words) {
            target.store(value, Ordering::Relaxed);
        }
        self.version.store(published_version, Ordering::Release);
        Ok(())
    }

    pub(super) fn read(&self) -> Result<Option<[u64; WORDS]>, SnapshotReadError> {
        let mut observed_version = self.version.load(Ordering::Acquire);
        for _ in 0..READ_ATTEMPTS {
            if observed_version == 0 {
                return Ok(None);
            }
            if observed_version & 1 == 0 {
                let words = core::array::from_fn(|index| self.words[index].load(Ordering::Relaxed));
                let verified_version = self.version.load(Ordering::Acquire);
                if verified_version == observed_version {
                    return Ok(Some(words));
                }
                observed_version = verified_version;
            } else {
                core::hint::spin_loop();
                observed_version = self.version.load(Ordering::Acquire);
            }
        }
        Err(SnapshotReadError {
            attempts: READ_ATTEMPTS,
            observed_version,
        })
    }
}

#[cfg(all(test, feature = "loom-model"))]
mod tests {
    use loom::{sync::Arc, thread};

    use super::AtomicSnapshot;

    #[test]
    fn reader_observes_one_complete_publication_during_replacement() {
        loom::model(|| {
            let snapshot = Arc::new(AtomicSnapshot::<2>::empty());
            snapshot.replace([1, 2]).expect("initial writer is unique");

            let writer_snapshot = Arc::clone(&snapshot);
            let writer = thread::spawn(move || {
                writer_snapshot
                    .replace([3, 4])
                    .expect("replacement writer is unique");
            });
            let observed = snapshot.read();
            writer.join().expect("writer did not panic");

            match observed {
                Ok(Some([1, 2] | [3, 4])) => {}
                Err(error) if error.attempts > 0 => {}
                other => panic!("reader observed a mixed snapshot: {other:?}"),
            }
            assert_eq!(snapshot.read(), Ok(Some([3, 4])));
        });
    }
}
