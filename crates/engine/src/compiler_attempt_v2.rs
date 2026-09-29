//! Opaque identity for one owner-created compiler staging attempt.
//!
//! This identity only permits in-memory handoff between a staged compiler result and a
//! future read-frontier observer. It is not a read-completeness proof and is never serialized.

use std::sync::{
    OnceLock,
    atomic::{AtomicU64, Ordering},
};

#[cfg(unix)]
use std::{fs::File, io::Read};

static NEXT_COMPILATION_ATTEMPT_ID: AtomicU64 = AtomicU64::new(1);
static COMPILATION_ATTEMPT_EPOCH: OnceLock<Result<[u8; 16], ()>> = OnceLock::new();

/// Unforgeable outside this crate, owner-minted identity for one compilation attempt.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub(crate) struct CompilationAttemptId {
    attempt_nonce: [u8; 16],
    process_id: u32,
    counter: u64,
}

impl std::fmt::Debug for CompilationAttemptId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CompilationAttemptId(<opaque>)")
    }
}

impl CompilationAttemptId {
    /// Mints an opaque cross-process identity for one in-memory compile attempt.
    /// Entropy failure or counter exhaustion fails closed.
    pub(crate) fn mint() -> Option<Self> {
        let attempt_nonce = *COMPILATION_ATTEMPT_EPOCH
            .get_or_init(|| {
                let mut nonce = [0; 16];
                fill_entropy(&mut nonce).map_err(|_| ())?;
                Ok(nonce)
            })
            .as_ref()
            .ok()?;
        let counter = NEXT_COMPILATION_ATTEMPT_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .ok()?;
        Some(Self::from_epoch_process_and_counter(
            attempt_nonce,
            std::process::id(),
            counter,
        ))
    }

    fn from_epoch_process_and_counter(
        attempt_nonce: [u8; 16],
        process_id: u32,
        counter: u64,
    ) -> Self {
        Self {
            attempt_nonce,
            process_id,
            counter,
        }
    }

    /// Stable diagnostic counter; this is not used as the identity or proof.
    pub(crate) const fn get(self) -> u64 {
        self.counter
    }

    pub(crate) fn update_hasher(self, hasher: &mut blake3::Hasher) {
        hasher.update(&self.attempt_nonce);
        hasher.update(&self.process_id.to_be_bytes());
        hasher.update(&self.counter.to_be_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::CompilationAttemptId;

    #[test]
    fn distinct_epochs_or_process_ids_separate_replayed_counters() {
        let prior_process = CompilationAttemptId::from_epoch_process_and_counter([0x31; 16], 11, 1);
        let restarted_process =
            CompilationAttemptId::from_epoch_process_and_counter([0x32; 16], 11, 1);
        assert_ne!(prior_process, restarted_process);

        let forked_child = CompilationAttemptId::from_epoch_process_and_counter([0x31; 16], 12, 1);
        assert_ne!(prior_process, forked_child);
    }
}

#[cfg(unix)]
fn fill_entropy(bytes: &mut [u8]) -> std::io::Result<()> {
    File::open("/dev/urandom")?.read_exact(bytes)
}

#[cfg(windows)]
fn fill_entropy(bytes: &mut [u8]) -> std::io::Result<()> {
    backend_platform::win32::random::fill(bytes)
}

#[cfg(not(any(unix, windows)))]
fn fill_entropy(_bytes: &mut [u8]) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "secure random source unavailable",
    ))
}
