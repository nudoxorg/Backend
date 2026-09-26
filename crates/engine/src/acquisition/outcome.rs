use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::identity::ID_BYTES;

/// Closed terminal/result algebra for every acquisition phase.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AcquisitionOutcome<T> {
    /// A verified object or published delta was returned.
    Hit(T),
    /// A signed/authoritative negative fact was returned.
    NegativeFact(NegativeFact),
    /// The caller should retry no earlier than the supplied time.
    RetryAt(RetryAt),
    /// The source circuit is open.
    CircuitOpen(CircuitOpen),
    /// The source could not be reached or completed within bounds.
    Unavailable(Unavailable),
    /// The configured source is deliberately offline.
    Offline(Offline),
    /// Policy or protocol rejected the request.
    Rejected(RejectReason),
    /// Persisted state or content failed integrity checks.
    Corrupt(CorruptReason),
    /// The caller cancelled its demand.
    Cancelled,
}

/// Why an acquisition was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RejectReason {
    /// The request is outside configured policy.
    Policy,
    /// The adapter returned malformed protocol data.
    Protocol,
    /// The request exceeded a configured bound.
    Bounds,
}
/// Why durable state or content was corrupt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CorruptReason {
    /// Hash did not match the claimed content.
    Integrity,
    /// Journal or root record was malformed.
    Journal,
}
/// Retry timestamp and attempt ordinal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryAt {
    /// Earliest retry instant in monotonic milliseconds from the process epoch.
    pub at_millis: u64,
    /// Retry attempt used for backoff accounting.
    pub attempt: u32,
}
/// Circuit-open observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CircuitOpen {
    /// Time after which a half-open probe may be admitted.
    pub until_millis: u64,
}
/// Unavailability observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Unavailable {
    /// Stable source identity.
    pub source: [u8; ID_BYTES],
}

/// Typed offline observation for one source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Offline {
    /// Stable source identity.
    pub source: [u8; ID_BYTES],
}

/// Typed negative fact retained by the negative cache.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NegativeFact {
    /// Exact negative fact kind.
    pub kind: NegativeFactKind,
    /// Authority that signed/observed the fact.
    pub authority: [u8; ID_BYTES],
    /// Proof digest from the source response.
    pub source_proof: [u8; ID_BYTES],
    /// Feed cursor or snapshot token at observation.
    pub cursor: [u8; ID_BYTES],
    /// Wall-clock observation timestamp.
    pub observed_at_millis: u64,
    /// Expiry timestamp.
    pub expires_at_millis: u64,
    /// Policy epoch under which this fact is valid.
    pub policy_epoch: u64,
}

/// Negative facts distinguish absence from transient failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NegativeFactKind {
    /// Source definitively has no matching coordinate.
    NotFound,
    /// Coordinate existed but was withdrawn.
    Yanked,
    /// Policy authority blocked the release for advisory reasons.
    AdvisoryBlocked,
    /// Source explicitly does not support the requested artifact.
    Unsupported,
}

impl NegativeFact {
    /// Returns whether the fact can be used at an observation time and epoch.
    #[must_use]
    pub const fn valid_at(self, now_millis: u64, policy_epoch: u64) -> bool {
        self.expires_at_millis > now_millis && self.policy_epoch == policy_epoch
    }
}

/// Bounded negative cache keyed by source and coordinate identity.
#[derive(Clone, Debug)]
pub struct NegativeCache {
    entries: Arc<Mutex<BTreeMap<[u8; ID_BYTES], NegativeFact>>>,
    capacity: usize,
}

impl NegativeCache {
    /// Creates a cache with a fixed entry bound.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: Arc::new(Mutex::new(BTreeMap::new())),
            capacity,
        }
    }
    /// Records one fact, evicting the lexicographically oldest key only when
    /// the configured bound is reached.
    pub fn record(&self, key: [u8; ID_BYTES], fact: NegativeFact) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if entries.len() >= self.capacity && !entries.contains_key(&key) {
            if let Some(oldest) = entries.keys().next().copied() {
                entries.remove(&oldest);
            }
        }
        if self.capacity != 0 {
            entries.insert(key, fact);
        }
    }
    /// Gets a currently valid fact. Expired facts are removed.
    pub fn get(
        &self,
        key: [u8; ID_BYTES],
        now_millis: u64,
        policy_epoch: u64,
    ) -> Option<NegativeFact> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let fact = entries.get(&key).copied();
        if fact.is_some_and(|fact| !fact.valid_at(now_millis, policy_epoch)) {
            entries.remove(&key);
            None
        } else {
            fact
        }
    }
}

pub(super) fn promote_void_outcome<T>(outcome: AcquisitionOutcome<()>) -> AcquisitionOutcome<T> {
    match outcome {
        AcquisitionOutcome::Hit(()) => unreachable!("void acquisition cannot hit"),
        AcquisitionOutcome::NegativeFact(fact) => AcquisitionOutcome::NegativeFact(fact),
        AcquisitionOutcome::RetryAt(retry) => AcquisitionOutcome::RetryAt(retry),
        AcquisitionOutcome::CircuitOpen(open) => AcquisitionOutcome::CircuitOpen(open),
        AcquisitionOutcome::Unavailable(unavailable) => {
            AcquisitionOutcome::Unavailable(unavailable)
        }
        AcquisitionOutcome::Offline(offline) => AcquisitionOutcome::Offline(offline),
        AcquisitionOutcome::Rejected(reason) => AcquisitionOutcome::Rejected(reason),
        AcquisitionOutcome::Corrupt(reason) => AcquisitionOutcome::Corrupt(reason),
        AcquisitionOutcome::Cancelled => AcquisitionOutcome::Cancelled,
    }
}

pub(super) fn promote_bytes_outcome<T>(
    outcome: AcquisitionOutcome<Arc<[u8]>>,
) -> AcquisitionOutcome<T> {
    match outcome {
        AcquisitionOutcome::Hit(_) => AcquisitionOutcome::Corrupt(CorruptReason::Journal),
        AcquisitionOutcome::NegativeFact(fact) => AcquisitionOutcome::NegativeFact(fact),
        AcquisitionOutcome::RetryAt(retry) => AcquisitionOutcome::RetryAt(retry),
        AcquisitionOutcome::CircuitOpen(open) => AcquisitionOutcome::CircuitOpen(open),
        AcquisitionOutcome::Unavailable(unavailable) => {
            AcquisitionOutcome::Unavailable(unavailable)
        }
        AcquisitionOutcome::Offline(offline) => AcquisitionOutcome::Offline(offline),
        AcquisitionOutcome::Rejected(reason) => AcquisitionOutcome::Rejected(reason),
        AcquisitionOutcome::Corrupt(reason) => AcquisitionOutcome::Corrupt(reason),
        AcquisitionOutcome::Cancelled => AcquisitionOutcome::Cancelled,
    }
}
