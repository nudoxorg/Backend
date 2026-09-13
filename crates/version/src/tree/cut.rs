//! Canonical key-anchored ordered Merkle trees.
//!
//! The tree format is independent of storage layout.  Leaves and branches are
//! rebuilt from sorted logical keys, schema tags, and the versioned cut policy;
//! equal bytes therefore produce equal relation roots regardless of insertion
//! order or physical packing.

use core::fmt;

use super::{CUT_HASH_DOMAIN, DEFAULT_MAX_ENCODED_BYTES};
use crate::{CANONICAL_CUT_POLICY_VERSION, CANONICAL_TREE_ABI, Relation};

// `V2NODE\0`, ABI/policy/kind/version/domain/type/level, and the outer body
// length prefix used by both leaf and branch nodes.
const NODE_OVERHEAD_BYTES: usize = 24;

// Count anchors alone cannot stabilize relations with large values: those
// nodes reach the hard byte cap before `min_entries`.  These derived byte
// targets give the same key anchor a proportional chance to close a page once
// the page is useful, while the hard cap remains an unconditional bound.
// They are part of CANONICAL_CUT_POLICY_VERSION and must change with it.
const MIN_ENCODED_BODY_BYTES: usize = DEFAULT_MAX_ENCODED_BYTES / 4;
const TARGET_ENCODED_BODY_BYTES: usize = DEFAULT_MAX_ENCODED_BYTES / 2;

/// Deterministic ordered-tree cut parameters.
///
/// The fields are the starting 64/256/1024 entry policy from the architecture
/// layout.  The ABI and policy version are fixed associated constants and are
/// included in every canonical node header and cut anchor.  Physical tuning
/// must use a new policy version so it cannot silently reuse a logical root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CutPolicy {
    /// Minimum entries before a natural boundary may be selected.
    pub min_entries: u16,
    /// Target entry count used to derive the natural-boundary probability.
    pub target_entries: u16,
    /// Hard maximum entries in one leaf or branch.
    pub max_entries: u16,
}

impl CutPolicy {
    /// ABI tag for this policy's canonical node format.
    pub const ABI: u8 = CANONICAL_TREE_ABI;
    /// Version of the key-anchored cut rule.
    pub const VERSION: u8 = CANONICAL_CUT_POLICY_VERSION;
    /// Hard encoded-byte limit for one canonical node.
    pub const MAX_ENCODED_BYTES: usize = DEFAULT_MAX_ENCODED_BYTES;

    /// Creates a policy value; validity is checked when boundaries are built.
    #[must_use]
    pub const fn new(min_entries: u16, target_entries: u16, max_entries: u16) -> Self {
        Self {
            min_entries,
            target_entries,
            max_entries,
        }
    }

    /// Returns the policy's ABI tag.
    #[must_use]
    pub const fn abi(self) -> u8 {
        Self::ABI
    }

    /// Returns the policy's canonical version.
    #[must_use]
    pub const fn version(self) -> u8 {
        Self::VERSION
    }

    /// Returns the hard encoded-byte node limit.
    #[must_use]
    pub const fn max_encoded_bytes(self) -> usize {
        Self::MAX_ENCODED_BYTES
    }

    /// Returns whether minimum, target, and maximum counts form a valid policy.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.min_entries != 0
            && self.min_entries <= self.target_entries
            && self.target_entries <= self.max_entries
    }

    fn validate(self) -> Result<(), CutError> {
        if !self.is_valid() {
            return Err(CutError::InvalidPolicy);
        }
        Ok(())
    }
}

/// Initial canonical cut policy used by bulk builders and path-copy updates.
pub const DEFAULT_CUT_POLICY: CutPolicy = CutPolicy {
    min_entries: 64,
    target_entries: 256,
    max_entries: 1024,
};

/// Failure while deriving canonical ordered-tree boundaries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CutError {
    /// Minimum, target, and maximum counts were not monotone and nonzero.
    InvalidPolicy,
    /// Input ordering or a checked arithmetic operation was invalid.
    Overflow,
    /// Input keys were not strictly increasing.
    UnsortedOrDuplicate,
    /// One encoded key cannot fit in a canonical node even by itself.
    OversizedKey,
    /// One encoded key/value or child reference cannot fit in a canonical
    /// node even by itself.
    OversizedEntry,
}

impl fmt::Display for CutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid canonical cut policy or key sequence: {self:?}")
    }
}

impl std::error::Error for CutError {}

/// Returns fixed target-sized canonical ranges under one shared cut policy.
///
/// # Errors
///
/// Returns [`CutError::InvalidPolicy`] for an invalid policy,
/// [`CutError::UnsortedOrDuplicate`] for malformed input ordering, or
/// [`CutError::Overflow`] when a checked boundary/count operation cannot be
/// represented.
pub fn cut_points<K: Ord>(keys: &[K], policy: CutPolicy) -> Result<Vec<usize>, CutError> {
    policy.validate()?;
    if keys.windows(2).any(|window| window[0] >= window[1]) {
        return Err(CutError::UnsortedOrDuplicate);
    }
    let target = usize::from(policy.target_entries);
    let max = usize::from(policy.max_entries);
    let mut output = Vec::new();
    let mut at = 0usize;
    while at < keys.len() {
        let next = at
            .checked_add(target)
            .ok_or(CutError::Overflow)?
            .min(keys.len());
        if next <= at || next - at > max {
            return Err(CutError::Overflow);
        }
        output.push(next);
        at = next;
        if output.len() > keys.len() {
            return Err(CutError::Overflow);
        }
    }
    Ok(output)
}

/// Returns relation-aware content-defined cuts under the versioned policy.
///
/// The relation schema, tree ABI/version, level, policy counts, and encoded
/// key anchor all participate in boundary selection. Natural boundaries react
/// independently to row count and encoded byte pressure, preserving stable
/// chunks for both tiny and large values. A hard maximum forces a boundary
/// even if no natural anchor appears. The function never uses unchecked
/// arithmetic or assumes that a hash prefix conversion can fail.
///
/// # Errors
///
/// Returns [`CutError::InvalidPolicy`] for an invalid policy,
/// [`CutError::UnsortedOrDuplicate`] for malformed key ordering, or
/// [`CutError::OversizedKey`] when one encoded key exceeds the node cap, or
/// [`CutError::Overflow`] when a checked boundary/count operation cannot be
/// represented.
pub fn anchored_cut_points<R: Relation>(
    keys: &[R::Key],
    policy: CutPolicy,
    level: u16,
) -> Result<Vec<usize>, CutError> {
    anchored_cut_points_refs::<R, _>(keys.iter(), policy, level)
}

/// Returns anchored cut points while borrowing an ordered key iterator.
///
/// This internal kernel seam is used by persistent bulk builders so deriving
/// boundaries does not clone all key values into a temporary projection.
pub(crate) fn anchored_cut_points_refs<'a, R, I>(
    keys: I,
    policy: CutPolicy,
    level: u16,
) -> Result<Vec<usize>, CutError>
where
    R: Relation + 'a,
    I: IntoIterator<Item = &'a R::Key>,
{
    anchored_cut_points_sized::<R, _>(keys.into_iter().map(|key| (key, 0usize)), policy, level)
}

/// Returns anchored cuts for branch children, accounting for each encoded
/// key plus the length-delimited 32-byte child commitment and row count.
pub(crate) fn anchored_cut_points_children<'a, R, I>(
    keys: I,
    policy: CutPolicy,
    level: u16,
) -> Result<Vec<usize>, CutError>
where
    R: Relation + 'a,
    I: IntoIterator<Item = &'a R::Key>,
{
    anchored_cut_points_sized::<R, _>(keys.into_iter().map(|key| (key, 48usize)), policy, level)
}

fn cut_anchor_word<R: Relation>(encoded_key: &[u8], policy: CutPolicy, level: u16) -> u64 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(CUT_HASH_DOMAIN);
    hasher.update(&[CANONICAL_TREE_ABI, CANONICAL_CUT_POLICY_VERSION]);
    hasher.update(&[R::DOMAIN]);
    hasher.update(&R::TYPE.to_be_bytes());
    hasher.update(&[R::VERSION]);
    hasher.update(&level.to_be_bytes());
    hasher.update(&policy.min_entries.to_be_bytes());
    hasher.update(&policy.target_entries.to_be_bytes());
    hasher.update(&policy.max_entries.to_be_bytes());
    hasher.update(&(encoded_key.len() as u64).to_be_bytes());
    hasher.update(encoded_key);
    let digest = hasher.finalize();
    let mut prefix = [0; size_of::<u64>()];
    prefix.copy_from_slice(&digest.as_bytes()[..size_of::<u64>()]);
    u64::from_be_bytes(prefix)
}

fn has_byte_anchor(
    word: u64,
    entry_bytes: usize,
    body_bytes: usize,
    target_encoded_bytes: u64,
) -> Result<bool, CutError> {
    if body_bytes < MIN_ENCODED_BODY_BYTES {
        return Ok(false);
    }
    let encoded_entry_bytes = u64::try_from(entry_bytes).map_err(|_| CutError::Overflow)?;
    let byte_weight = encoded_entry_bytes.min(target_encoded_bytes);
    let threshold = (u64::MAX / target_encoded_bytes)
        .checked_mul(byte_weight)
        .ok_or(CutError::Overflow)?;
    Ok(word <= threshold)
}

/// Returns anchored cuts for entries whose second item is the additional
/// encoded body size after the key field. The node header and length framing
/// are included in every boundary decision.
pub(crate) fn anchored_cut_points_sized<'a, R, I>(
    entries: I,
    policy: CutPolicy,
    level: u16,
) -> Result<Vec<usize>, CutError>
where
    R: Relation + 'a,
    I: IntoIterator<Item = (&'a R::Key, usize)>,
{
    policy.validate()?;
    let minimum = usize::from(policy.min_entries);
    let target = u64::from(policy.target_entries);
    let maximum = usize::from(policy.max_entries);
    let count_cut_threshold = u64::MAX / target;
    let target_encoded_bytes =
        u64::try_from(TARGET_ENCODED_BODY_BYTES).map_err(|_| CutError::Overflow)?;
    let mut output = Vec::new();
    let mut start = 0usize;
    let mut total = 0usize;
    let mut encoded_bytes = NODE_OVERHEAD_BYTES;
    let mut previous = None;
    let mut encoded_key = Vec::new();

    for (index, (key, extra_bytes)) in entries.into_iter().enumerate() {
        if previous.is_some_and(|previous: &R::Key| previous >= key) {
            return Err(CutError::UnsortedOrDuplicate);
        }
        previous = Some(key);
        total = index + 1;
        encoded_key.clear();
        R::encode_key(key, &mut encoded_key);
        let entry_bytes = 8usize
            .checked_add(encoded_key.len())
            .and_then(|bytes| bytes.checked_add(extra_bytes))
            .ok_or(CutError::Overflow)?;
        if NODE_OVERHEAD_BYTES
            .checked_add(entry_bytes)
            .ok_or(CutError::Overflow)?
            > policy.max_encoded_bytes()
        {
            return Err(if extra_bytes == 0 {
                CutError::OversizedKey
            } else {
                CutError::OversizedEntry
            });
        }
        if encoded_bytes
            .checked_add(entry_bytes)
            .ok_or(CutError::Overflow)?
            > policy.max_encoded_bytes()
        {
            if index == start {
                return Err(CutError::OversizedEntry);
            }
            output.push(index);
            start = index;
            encoded_bytes = NODE_OVERHEAD_BYTES;
        }
        encoded_bytes = encoded_bytes
            .checked_add(entry_bytes)
            .ok_or(CutError::Overflow)?;
        let count = index - start + 1;
        let body_bytes = encoded_bytes
            .checked_sub(NODE_OVERHEAD_BYTES)
            .ok_or(CutError::Overflow)?;
        if count < minimum && body_bytes < MIN_ENCODED_BODY_BYTES {
            continue;
        }
        let word = cut_anchor_word::<R>(&encoded_key, policy, level);
        let count_anchor = count >= minimum && word <= count_cut_threshold;
        let byte_anchor = has_byte_anchor(word, entry_bytes, body_bytes, target_encoded_bytes)?;
        let natural_cut = count_anchor || byte_anchor;
        if count >= maximum || natural_cut {
            let boundary = index + 1;
            if boundary <= start {
                return Err(CutError::Overflow);
            }
            output.push(boundary);
            start = boundary;
            encoded_bytes = NODE_OVERHEAD_BYTES;
            if output.len() > total {
                return Err(CutError::Overflow);
            }
        }
    }
    if start < total {
        output.push(total);
    }
    Ok(output)
}

/// Returns cuts for complete leaf entries, accounting for encoded key and
/// value bytes rather than entry counts alone.
pub(crate) fn anchored_cut_points_items<'a, R, I>(
    items: I,
    policy: CutPolicy,
    level: u16,
) -> Result<Vec<usize>, CutError>
where
    R: Relation + 'a,
    I: IntoIterator<Item = &'a (R::Key, R::Value)>,
{
    let sized = items.into_iter().map(|(key, value)| {
        let mut encoded_value = Vec::new();
        R::encode_value(value, &mut encoded_value);
        (key, 8usize.saturating_add(encoded_value.len()))
    });
    anchored_cut_points_sized::<R, _>(sized, policy, level)
}
