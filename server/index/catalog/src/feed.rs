//! Typed, bounded identity and opaque progress state for one registry feed.

use server_index_ingest::Checkpoint;

/// Maximum UTF-8 byte length admitted for a durable feed identity.
pub const MAX_FEED_ID_BYTES: usize = 160;
/// Maximum UTF-8 byte length admitted for an opaque registry cursor or validator.
pub const MAX_FEED_TOKEN_BYTES: usize = 2_048;

/// A validated stable identity for one independently polled registry feed.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct FeedIdentity(String);

/// Exact reason a feed identity was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FeedIdentityFault {
    /// The identifier was empty.
    Empty,
    /// The identifier exceeded its durable bound.
    TooLong,
    /// The identifier included a control byte or an unapproved character.
    InvalidCharacter,
}

impl FeedIdentity {
    /// Admits a stable lower-ASCII namespaced feed identity.
    pub fn new(value: impl Into<String>) -> Result<Self, FeedIdentityFault> {
        let value = value.into();
        if value.is_empty() {
            return Err(FeedIdentityFault::Empty);
        }
        if value.len() > MAX_FEED_ID_BYTES {
            return Err(FeedIdentityFault::TooLong);
        }
        if !value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'-' | b'/' | b':' | b'_')
        }) {
            return Err(FeedIdentityFault::InvalidCharacter);
        }
        Ok(Self(value))
    }

    /// Returns the exact stable feed identity for durable lookup.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A bounded opaque cursor for one registry feed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeedCursor(String);

/// Exact reason an opaque cursor or validator was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FeedCursorFault {
    /// The token was empty.
    Empty,
    /// The token exceeded its durable bound.
    TooLong,
    /// The token contained a control character.
    ControlCharacter,
}

impl FeedCursor {
    /// Admits nonempty printable UTF-8 source state without interpreting its grammar.
    pub fn new(value: impl Into<String>) -> Result<Self, FeedCursorFault> {
        let value = value.into();
        if value.is_empty() {
            return Err(FeedCursorFault::Empty);
        }
        if value.len() > MAX_FEED_TOKEN_BYTES {
            return Err(FeedCursorFault::TooLong);
        }
        if value.bytes().any(|byte| byte.is_ascii_control()) {
            return Err(FeedCursorFault::ControlCharacter);
        }
        Ok(Self(value))
    }

    /// Returns the opaque source token exactly as persisted.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A bounded opaque conditional validator for exactly one feed resource.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeedValidator(String);

/// Exact reason a feed validator was rejected.
pub type FeedValidatorFault = FeedCursorFault;

impl FeedValidator {
    /// Admits one nonempty printable response validator without interpreting its grammar.
    pub fn new(value: impl Into<String>) -> Result<Self, FeedValidatorFault> {
        FeedCursor::new(value).map(|cursor| Self(cursor.0))
    }

    /// Returns the opaque validator exactly as persisted.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Fixed-width content identity for one stable registry snapshot body.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FeedSnapshotId([u8; 32]);

impl FeedSnapshotId {
    /// Admits the SHA-256 digest computed over the exact bounded source body.
    pub const fn from_sha256(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the exact source-body digest for durable encoding.
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// Durable source progress for one feed. `cursor` and `validator` are independent facts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeedCheckpoint {
    /// Stable identity of the source feed.
    pub feed: FeedIdentity,
    /// Monotonic successful-page progress.
    pub checkpoint: Checkpoint,
    /// Opaque upstream cursor for the next request, if the source uses one.
    pub cursor: Option<FeedCursor>,
    /// Opaque conditional request validator, such as an ETag or ref digest.
    pub validator: Option<FeedValidator>,
    /// Monotonic full-snapshot cycle; incomplete cycles never authorize removals.
    pub cycle: u64,
    /// Content reference of the stable snapshot currently being chunked.
    pub snapshot: Option<FeedSnapshotId>,
    /// Number of source rows durably accounted for in `snapshot`.
    pub offset: u64,
}

/// Exact reason a successor feed state cannot represent one bounded atomic apply.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FeedTransitionFault {
    /// A non-snapshot page attempted to alter cycle, snapshot, or offset state.
    NonSnapshotState,
    /// A fresh snapshot did not advance exactly one cycle from a completed state.
    NewCycle,
    /// A continuation did not retain the same stable snapshot reference.
    SnapshotIdentity,
    /// A successor offset did not account for exactly this page's observations.
    Offset,
    /// The `complete` argument disagreed with the successor's terminal snapshot shape.
    Completion,
    /// The stored current state has an impossible no-snapshot nonzero offset.
    CurrentState,
}

impl FeedCheckpoint {
    /// Constructs the initial, not-yet-applied state for `feed`.
    pub const fn initial(feed: FeedIdentity) -> Self {
        Self {
            feed,
            checkpoint: Checkpoint {
                sequence: 0,
                page: 0,
            },
            cursor: None,
            validator: None,
            cycle: 0,
            snapshot: None,
            offset: 0,
        }
    }
}

/// Checks that an atomically applied snapshot page cannot forge completion or skip rows.
pub(crate) fn validate_snapshot_transition(
    current: &FeedCheckpoint,
    next: &FeedCheckpoint,
    observation_count: usize,
    complete: bool,
) -> Result<(), FeedTransitionFault> {
    if (complete && (next.snapshot.is_some() || next.offset != 0))
        || (!complete && (next.snapshot.is_none() || next.offset == 0))
    {
        return Err(FeedTransitionFault::Completion);
    }
    if (current.snapshot.is_none() && current.offset != 0)
        || (current.snapshot.is_some() && current.offset == 0)
    {
        return Err(FeedTransitionFault::CurrentState);
    }
    match &current.snapshot {
        None => {
            if next.cycle
                != current
                    .cycle
                    .checked_add(1)
                    .ok_or(FeedTransitionFault::NewCycle)?
            {
                return Err(FeedTransitionFault::NewCycle);
            }
            if !complete {
                let expected =
                    u64::try_from(observation_count).map_err(|_| FeedTransitionFault::Offset)?;
                if expected == 0 || next.offset != expected {
                    return Err(FeedTransitionFault::Offset);
                }
            }
        }
        Some(snapshot) => {
            if next.cycle == current.cycle {
                if complete {
                    if observation_count == 0 {
                        return Err(FeedTransitionFault::Offset);
                    }
                } else {
                    if next.snapshot.as_ref() != Some(snapshot) {
                        return Err(FeedTransitionFault::SnapshotIdentity);
                    }
                    let added = u64::try_from(observation_count)
                        .map_err(|_| FeedTransitionFault::Offset)?;
                    let expected = current
                        .offset
                        .checked_add(added)
                        .ok_or(FeedTransitionFault::Offset)?;
                    if added == 0 || next.offset != expected {
                        return Err(FeedTransitionFault::Offset);
                    }
                }
            } else if next.cycle
                == current
                    .cycle
                    .checked_add(1)
                    .ok_or(FeedTransitionFault::NewCycle)?
            {
                if !complete {
                    let added = u64::try_from(observation_count)
                        .map_err(|_| FeedTransitionFault::Offset)?;
                    if next.snapshot.as_ref() == Some(snapshot)
                        || added == 0
                        || next.offset != added
                    {
                        return Err(FeedTransitionFault::Offset);
                    }
                }
            } else {
                return Err(FeedTransitionFault::NewCycle);
            }
        }
    }
    Ok(())
}

/// Checks that a non-snapshot page leaves snapshot-cycle state untouched.
pub(crate) fn validate_non_snapshot_transition(
    current: &FeedCheckpoint,
    next: &FeedCheckpoint,
) -> Result<(), FeedTransitionFault> {
    if current.cycle != next.cycle
        || current.snapshot != next.snapshot
        || current.offset != next.offset
    {
        return Err(FeedTransitionFault::NonSnapshotState);
    }
    Ok(())
}
