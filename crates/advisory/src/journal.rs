use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::model::{Advisory, AliasGraph, AliasGraphError, CanonicalAdvisoryId, Evidence};

/// One source-backed withdrawal event retained for audit and replay.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WithdrawalRecord {
    /// Withdrawal timestamp supplied by the authority.
    pub withdrawn: String,
    /// Evidence which authenticated the withdrawal claim.
    pub evidence: Evidence,
}

/// Incremental operation on the advisory object set.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AdvisoryDelta {
    /// Insert or replace one fully parsed object with its source evidence.
    Upsert(Advisory),
    /// Record a withdrawal while retaining the previous object and evidence.
    Withdraw {
        /// Canonical object to update.
        key: CanonicalAdvisoryId,
        /// Withdrawal timestamp from the source.
        withdrawn: String,
        /// Freshness/verification evidence for this claim.
        evidence: Evidence,
    },
    /// Remove a source object while retaining a durable tombstone.
    Delete {
        /// Object identity.
        key: CanonicalAdvisoryId,
        /// Stable reason token.
        reason: String,
    },
    /// Explicitly retain a tombstone without requiring an object body.
    Tombstone {
        /// Object identity.
        key: CanonicalAdvisoryId,
        /// Stable reason token.
        reason: String,
    },
}

/// Full snapshot or incremental source update.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SyncMode {
    /// Only supplied operations are authoritative.
    Delta,
    /// Missing objects may be removed after a complete snapshot.
    Snapshot,
}

/// `ETag` and `Last-Modified` state for one source frontier.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FeedFreshness {
    /// Source `ETag`, retained verbatim for conditional GET.
    pub etag: Option<String>,
    /// Source `Last-Modified` value, retained verbatim for conditional GET.
    pub last_modified: Option<String>,
    /// Local wall-clock observation in seconds.
    pub observed_at: u64,
    /// Whether this response carried a body or validated the previous body.
    pub not_modified: bool,
}

/// Monotonic durable checkpoint after one sync attempt, including empty attempts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Sequence advances for every accepted sync, including empty withdrawal-only and 304 runs.
    pub sequence: u64,
    /// Digest of the active object frontier and tombstones.
    pub digest: [u8; 32],
    /// Feed freshness evidence.
    pub freshness: FeedFreshness,
}

/// Sync transaction applied atomically to one advisory frontier.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdvisorySync {
    /// Whether entries represent a delta or snapshot.
    pub mode: SyncMode,
    /// Snapshot completeness marker.
    pub complete: bool,
    /// Entries admitted in this transaction.
    pub entries: Vec<AdvisoryDelta>,
    /// Freshness evidence for this transaction.
    pub freshness: FeedFreshness,
}

/// Durable advisory frontier with versioned object and tombstone indexes.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdvisoryJournal {
    active: BTreeMap<CanonicalAdvisoryId, Advisory>,
    tombstones: BTreeMap<CanonicalAdvisoryId, String>,
    withdrawals: BTreeMap<CanonicalAdvisoryId, Box<[WithdrawalRecord]>>,
    aliases: AliasGraph,
    checkpoint: Option<Checkpoint>,
}

/// Journal admission failures are atomic: no partially applied sync is observable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdvisoryJournalError {
    /// A delta refers to an object which does not exist.
    Missing(CanonicalAdvisoryId),
    /// A source alias/package/range claim conflicts with an existing claim.
    Conflict(AliasGraphError),
    /// The source attempted to complete a snapshot with an invalid identity.
    InvalidSnapshot,
    /// One source snapshot repeated the same native identity.
    Duplicate(CanonicalAdvisoryId),
    /// An internal transaction invariant was violated.
    InvariantViolation,
    /// Monotonic sequence space was exhausted.
    SequenceOverflow,
}

impl AdvisoryJournal {
    /// Creates an empty source frontier.
    #[must_use]
    pub fn new() -> Self {
        Self {
            active: BTreeMap::new(),
            tombstones: BTreeMap::new(),
            withdrawals: BTreeMap::new(),
            aliases: AliasGraph::new(),
            checkpoint: None,
        }
    }

    /// Returns one active advisory by canonical identity.
    #[must_use]
    pub fn get(&self, key: &CanonicalAdvisoryId) -> Option<&Advisory> {
        self.active.get(key)
    }

    /// Returns the current checkpoint.
    #[must_use]
    pub const fn checkpoint(&self) -> Option<&Checkpoint> {
        self.checkpoint.as_ref()
    }

    /// Returns the complete withdrawal history for one canonical object.
    #[must_use]
    pub fn withdrawal_history(&self, key: &CanonicalAdvisoryId) -> &[WithdrawalRecord] {
        self.withdrawals.get(key).map_or(&[], Box::as_ref)
    }

    /// Returns the durable reason for a deleted or snapshot-omitted object.
    #[must_use]
    pub fn tombstone_reason(&self, key: &CanonicalAdvisoryId) -> Option<&str> {
        self.tombstones.get(key).map(String::as_str)
    }

    /// Number of active objects at this frontier.
    #[must_use]
    pub fn active_len(&self) -> usize {
        self.active.len()
    }

    /// Number of deletion tombstones retained for replay/audit.
    #[must_use]
    pub fn tombstone_len(&self) -> usize {
        self.tombstones.len()
    }

    /// Iterates active advisory objects in canonical identity order.
    pub fn iter(&self) -> impl Iterator<Item = &Advisory> {
        self.active.values()
    }

    /// Applies a sync atomically and advances the checkpoint even when entries are empty.
    ///
    /// # Errors
    ///
    /// Returns a typed conflict or missing-object error without modifying this journal.
    pub fn apply(&mut self, sync: AdvisorySync) -> Result<&Checkpoint, AdvisoryJournalError> {
        let mut candidate = self.clone();
        let mut seen = BTreeSet::new();
        let mut seen_native = BTreeSet::new();
        for entry in sync.entries {
            match entry {
                AdvisoryDelta::Upsert(mut advisory) => {
                    let native = advisory.key.native.id.clone();
                    if !seen_native.insert(native) {
                        return Err(AdvisoryJournalError::Duplicate(
                            advisory.key.canonical.clone(),
                        ));
                    }
                    let canonical = candidate
                        .aliases
                        .admit(&advisory)
                        .map_err(AdvisoryJournalError::Conflict)?;
                    advisory.key.canonical = canonical.clone();
                    seen.insert(canonical.clone());
                    candidate.tombstones.remove(&canonical);
                    candidate.active.insert(canonical, advisory);
                }
                AdvisoryDelta::Withdraw {
                    key,
                    withdrawn,
                    evidence,
                } => {
                    let advisory = candidate
                        .active
                        .get_mut(&key)
                        .ok_or_else(|| AdvisoryJournalError::Missing(key.clone()))?;
                    let record = WithdrawalRecord {
                        withdrawn: withdrawn.clone(),
                        evidence: evidence.clone(),
                    };
                    advisory.withdrawn = Some(withdrawn);
                    advisory.evidence = evidence;
                    let history = candidate.withdrawals.entry(key.clone()).or_default();
                    let mut next = history.to_vec();
                    next.push(record);
                    *history = next.into_boxed_slice();
                    seen.insert(key);
                }
                AdvisoryDelta::Delete { key, reason }
                | AdvisoryDelta::Tombstone { key, reason } => {
                    candidate.active.remove(&key);
                    candidate.tombstones.insert(key, reason);
                }
            }
        }
        if sync.mode == SyncMode::Snapshot && sync.complete && !sync.freshness.not_modified {
            let removed = candidate
                .active
                .keys()
                .filter(|key| !seen.contains(*key))
                .cloned()
                .collect::<Vec<_>>();
            for key in removed {
                candidate.active.remove(&key);
                candidate
                    .tombstones
                    .entry(key)
                    .or_insert_with(|| "snapshot-omitted".to_owned());
            }
        } else if sync.mode == SyncMode::Snapshot && (!sync.complete || sync.freshness.not_modified)
        {
            // An incomplete snapshot is only a page.  Removing unseen objects would turn a
            // transient transport failure into a data-loss delta. A 304 is a validation of the
            // prior complete body and therefore has the same no-removal semantics.
        }
        let sequence = candidate
            .checkpoint
            .as_ref()
            .map_or(0, |checkpoint| checkpoint.sequence)
            .checked_add(1)
            .ok_or(AdvisoryJournalError::SequenceOverflow)?;
        candidate.checkpoint = Some(Checkpoint {
            sequence,
            digest: candidate.digest(),
            freshness: sync.freshness,
        });
        *self = candidate;
        self.checkpoint
            .as_ref()
            .ok_or(AdvisoryJournalError::InvariantViolation)
    }

    fn digest(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"nudox.advisory.frontier.v1\0");
        for (key, advisory) in &self.active {
            hasher.update(key.0.as_bytes());
            if let Ok(bytes) = serde_json::to_vec(advisory) {
                hasher.update(&bytes);
            }
        }
        for (key, reason) in &self.tombstones {
            hasher.update(key.0.as_bytes());
            hasher.update(reason.as_bytes());
        }
        for (key, history) in &self.withdrawals {
            hasher.update(key.0.as_bytes());
            for record in history {
                if let Ok(bytes) = serde_json::to_vec(record) {
                    hasher.update(&bytes);
                }
            }
        }
        *hasher.finalize().as_bytes()
    }
}

impl AliasGraph {
    fn new() -> Self {
        Self::default()
    }
}
