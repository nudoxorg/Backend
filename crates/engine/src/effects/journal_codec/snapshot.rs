//! Typed effect snapshots and bounded snapshot codecs.

use super::super::state::{AmbiguousReason, EffectError, EffectFence};
use super::EffectLog;
use super::record::{put_bytes, read_bytes_ref, read_fixed, reason_from_tag, reason_tag};
use crate::journal::{
    ChainHash, JournalCheckpoint, JournalCodec, JournalDomain, JournalError, JournalReceipt,
};
use blake3::Hasher;

pub(super) const SNAPSHOT_FORMAT: u8 = 1;
pub(super) const SNAPSHOT_TAG: u8 = 1;
pub(super) const SNAPSHOT_MAX_ENTRIES: usize = 1_000_000;
pub(super) const SNAPSHOT_MAX_BYTES: usize = 64 * 1024 * 1024;
pub(super) const SNAPSHOT_HASH_DOMAIN: &[u8] = b"backend.engine.effect.snapshot.v2\0";

/// Bounds used while encoding or loading an effect checkpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EffectSnapshotLimits {
    /// Maximum number of effect entries retained by one snapshot.
    pub max_entries: usize,
    /// Maximum encoded snapshot payload, excluding the journal frame header.
    pub max_bytes: usize,
}

impl Default for EffectSnapshotLimits {
    fn default() -> Self {
        Self {
            max_entries: SNAPSHOT_MAX_ENTRIES,
            max_bytes: SNAPSHOT_MAX_BYTES,
        }
    }
}

pub(super) fn validate_effect_snapshot_limits(
    limits: EffectSnapshotLimits,
) -> Result<(), EffectError> {
    if limits.max_entries > SNAPSHOT_MAX_ENTRIES || limits.max_bytes > SNAPSHOT_MAX_BYTES {
        return Err(EffectError::Journal(JournalError::Bounds));
    }
    Ok(())
}

/// Fixed identity of the atomically selected effect snapshot frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EffectSnapshotReceipt {
    /// Byte offset of the selected snapshot frame.
    pub offset: u64,
    /// Sequence of the selected snapshot frame.
    pub sequence: u64,
    /// Hash-chain tip carried by the selected frame.
    pub chain: [u8; 32],
    /// Content identity carried by the selected frame.
    pub record: [u8; 32],
}

pub(super) fn encode_snapshot_receipt(receipt: EffectSnapshotReceipt, output: &mut Vec<u8>) {
    output.extend_from_slice(&receipt.offset.to_be_bytes());
    output.extend_from_slice(&receipt.sequence.to_be_bytes());
    output.extend_from_slice(&receipt.chain);
    output.extend_from_slice(&receipt.record);
}

pub(super) fn decode_snapshot_receipt(
    bytes: &[u8],
    at: &mut usize,
) -> Result<EffectSnapshotReceipt, JournalError> {
    Ok(EffectSnapshotReceipt {
        offset: u64::from_be_bytes(read_fixed::<8>(bytes, at)?),
        sequence: u64::from_be_bytes(read_fixed::<8>(bytes, at)?),
        chain: read_fixed::<32>(bytes, at)?,
        record: read_fixed::<32>(bytes, at)?,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct EffectSnapshotLog;

impl JournalDomain for EffectSnapshotLog {
    const DOMAIN: u8 = 0x81;
    const TYPE: u16 = 0x0005;
    const VERSION: u8 = 1;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct StoredCheckpoint {
    pub(super) offset: u64,
    pub(super) sequence: u64,
    pub(super) chain: [u8; 32],
    pub(super) record: [u8; 32],
}

impl StoredCheckpoint {
    pub(super) fn from_journal(checkpoint: JournalCheckpoint<EffectLog>) -> Self {
        Self {
            offset: checkpoint.offset,
            sequence: checkpoint.sequence,
            chain: *checkpoint.chain.as_bytes(),
            record: *checkpoint.record.as_bytes(),
        }
    }

    pub(super) fn journal(self) -> JournalCheckpoint<EffectLog> {
        JournalCheckpoint {
            offset: self.offset,
            sequence: self.sequence,
            chain: ChainHash::from_bytes(self.chain),
            record: crate::schema::RecordId::from_bytes(self.record),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RawSnapshotEntry {
    pub(super) key: [u8; 32],
    pub(super) intent: Vec<u8>,
    pub(super) state: RawSnapshotState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum RawSnapshotState {
    Prepared,
    Ambiguous {
        fence: EffectFence,
        ordinal: u64,
        reason: AmbiguousReason,
    },
    Confirmed {
        receipt: Vec<u8>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RawEffectSnapshot {
    pub(super) base: Option<StoredCheckpoint>,
    pub(super) last_ordinal: u64,
    pub(super) root: [u8; 32],
    pub(super) entries: Vec<RawSnapshotEntry>,
}

fn snapshot_digest(bytes: &[u8], root_offset: usize) -> Result<[u8; 32], JournalError> {
    let root_end = root_offset.checked_add(32).ok_or(JournalError::Bounds)?;
    if root_end > bytes.len() {
        return Err(JournalError::Corrupt("snapshot root"));
    }
    let mut hasher = Hasher::new();
    hasher.update(SNAPSHOT_HASH_DOMAIN);
    hasher.update(&bytes[..root_offset]);
    hasher.update(&[0; 32]);
    hasher.update(&bytes[root_end..]);
    Ok(*hasher.finalize().as_bytes())
}

pub(super) fn encode_snapshot(record: &RawEffectSnapshot, output: &mut Vec<u8>) {
    output.clear();
    output.push(SNAPSHOT_TAG);
    output.push(SNAPSHOT_FORMAT);
    match record.base {
        Some(base) => {
            output.push(1);
            output.extend_from_slice(&base.offset.to_be_bytes());
            output.extend_from_slice(&base.sequence.to_be_bytes());
            output.extend_from_slice(&base.chain);
            output.extend_from_slice(&base.record);
        }
        None => output.push(0),
    }
    output.extend_from_slice(&record.last_ordinal.to_be_bytes());
    let root_offset = output.len();
    output.extend_from_slice(&[0; 32]);
    output.extend_from_slice(&(record.entries.len() as u64).to_be_bytes());
    for entry in &record.entries {
        output.extend_from_slice(&entry.key);
        put_bytes(output, &entry.intent);
        match &entry.state {
            RawSnapshotState::Prepared => output.push(1),
            RawSnapshotState::Ambiguous {
                fence,
                ordinal,
                reason,
            } => {
                output.push(2);
                output.extend_from_slice(&fence.as_bytes());
                output.extend_from_slice(&ordinal.to_be_bytes());
                output.push(reason_tag(*reason));
            }
            RawSnapshotState::Confirmed { receipt } => {
                output.push(3);
                put_bytes(output, receipt);
            }
        }
    }
    if let Ok(root) = snapshot_digest(output, root_offset) {
        output[root_offset..root_offset + 32].copy_from_slice(&root);
    }
}

pub(super) fn decode_snapshot(bytes: &[u8]) -> Result<RawEffectSnapshot, JournalError> {
    validate_snapshot_limits(bytes.len())?;
    let mut at = 0;
    let base = decode_snapshot_header(bytes, &mut at)?;
    let last_ordinal = u64::from_be_bytes(read_fixed::<8>(bytes, &mut at)?);
    let root_offset = at;
    let root = read_fixed::<32>(bytes, &mut at)?;
    let count = snapshot_count(bytes, &mut at)?;
    if base.is_none() && count != 0 {
        return Err(JournalError::Corrupt("snapshot checkpoint"));
    }
    let mut entries = Vec::with_capacity(count);
    let mut previous_key = None;
    for _ in 0..count {
        let entry = decode_snapshot_entry(bytes, &mut at, &mut previous_key)?;
        entries.push(entry);
    }
    if at != bytes.len() {
        return Err(JournalError::Corrupt("snapshot trailing bytes"));
    }
    if snapshot_digest(bytes, root_offset)? != root {
        return Err(JournalError::Corrupt("snapshot root"));
    }
    Ok(RawEffectSnapshot {
        base,
        last_ordinal,
        root,
        entries,
    })
}

fn decode_snapshot_header(
    bytes: &[u8],
    at: &mut usize,
) -> Result<Option<StoredCheckpoint>, JournalError> {
    if *bytes
        .get(*at)
        .ok_or(JournalError::Corrupt("snapshot tag"))?
        != SNAPSHOT_TAG
    {
        return Err(JournalError::Corrupt("snapshot tag"));
    }
    *at += 1;
    if *bytes
        .get(*at)
        .ok_or(JournalError::Corrupt("snapshot format"))?
        != SNAPSHOT_FORMAT
    {
        return Err(JournalError::Corrupt("snapshot format"));
    }
    *at += 1;
    match *bytes
        .get(*at)
        .ok_or(JournalError::Corrupt("snapshot base"))?
    {
        0 => {
            *at += 1;
            Ok(None)
        }
        1 => {
            *at += 1;
            Ok(Some(StoredCheckpoint {
                offset: u64::from_be_bytes(read_fixed::<8>(bytes, at)?),
                sequence: u64::from_be_bytes(read_fixed::<8>(bytes, at)?),
                chain: read_fixed::<32>(bytes, at)?,
                record: read_fixed::<32>(bytes, at)?,
            }))
        }
        _ => Err(JournalError::Corrupt("snapshot base")),
    }
}

fn snapshot_count(bytes: &[u8], at: &mut usize) -> Result<usize, JournalError> {
    let count = usize::try_from(u64::from_be_bytes(read_fixed::<8>(bytes, at)?))
        .map_err(|_| JournalError::Bounds)?;
    if count > SNAPSHOT_MAX_ENTRIES {
        return Err(JournalError::Bounds);
    }
    Ok(count)
}

fn decode_snapshot_entry(
    bytes: &[u8],
    at: &mut usize,
    previous_key: &mut Option<[u8; 32]>,
) -> Result<RawSnapshotEntry, JournalError> {
    let key = read_fixed::<32>(bytes, at)?;
    if previous_key.is_some_and(|previous| key <= previous) {
        return Err(JournalError::Corrupt("snapshot order"));
    }
    *previous_key = Some(key);
    let intent = read_bytes_ref(bytes, at)?.to_vec();
    let state = decode_snapshot_state(bytes, at)?;
    Ok(RawSnapshotEntry { key, intent, state })
}

fn decode_snapshot_state(bytes: &[u8], at: &mut usize) -> Result<RawSnapshotState, JournalError> {
    match *bytes
        .get(*at)
        .ok_or(JournalError::Corrupt("snapshot phase"))?
    {
        1 => {
            *at += 1;
            Ok(RawSnapshotState::Prepared)
        }
        2 => {
            *at += 1;
            let fence = EffectFence(read_fixed::<32>(bytes, at)?);
            let ordinal = u64::from_be_bytes(read_fixed::<8>(bytes, at)?);
            let reason = reason_from_tag(
                *bytes
                    .get(*at)
                    .ok_or(JournalError::Corrupt("snapshot reason"))?,
            )?;
            *at += 1;
            Ok(RawSnapshotState::Ambiguous {
                fence,
                ordinal,
                reason,
            })
        }
        3 => {
            *at += 1;
            Ok(RawSnapshotState::Confirmed {
                receipt: read_bytes_ref(bytes, at)?.to_vec(),
            })
        }
        _ => Err(JournalError::Corrupt("snapshot phase")),
    }
}

/// Checks a snapshot's wire grammar and root without constructing any entry
/// values.  Journal scans invoke this before replay, where decoding every
/// frame must not allocate an owned history record just to authenticate it.
pub(super) fn validate_snapshot(bytes: &[u8]) -> Result<(), JournalError> {
    validate_snapshot_limits(bytes.len())?;
    let mut at = 0;
    if *bytes.get(at).ok_or(JournalError::Corrupt("snapshot tag"))? != SNAPSHOT_TAG {
        return Err(JournalError::Corrupt("snapshot tag"));
    }
    at += 1;
    if *bytes
        .get(at)
        .ok_or(JournalError::Corrupt("snapshot format"))?
        != SNAPSHOT_FORMAT
    {
        return Err(JournalError::Corrupt("snapshot format"));
    }
    at += 1;
    let has_base = match *bytes
        .get(at)
        .ok_or(JournalError::Corrupt("snapshot base"))?
    {
        0 => {
            at += 1;
            false
        }
        1 => {
            at += 1;
            let _ = read_fixed::<8>(bytes, &mut at)?;
            let _ = read_fixed::<8>(bytes, &mut at)?;
            let _ = read_fixed::<32>(bytes, &mut at)?;
            let _ = read_fixed::<32>(bytes, &mut at)?;
            true
        }
        _ => return Err(JournalError::Corrupt("snapshot base")),
    };
    let _ = read_fixed::<8>(bytes, &mut at)?;
    let root_offset = at;
    let root = read_fixed::<32>(bytes, &mut at)?;
    let count = usize::try_from(u64::from_be_bytes(read_fixed::<8>(bytes, &mut at)?))
        .map_err(|_| JournalError::Bounds)?;
    if count > SNAPSHOT_MAX_ENTRIES {
        return Err(JournalError::Bounds);
    }
    if !has_base && count != 0 {
        return Err(JournalError::Corrupt("snapshot checkpoint"));
    }
    let mut previous_key = None;
    for _ in 0..count {
        let key = read_fixed::<32>(bytes, &mut at)?;
        if previous_key.is_some_and(|previous| key <= previous) {
            return Err(JournalError::Corrupt("snapshot order"));
        }
        previous_key = Some(key);
        let _ = read_bytes_ref(bytes, &mut at)?;
        match *bytes
            .get(at)
            .ok_or(JournalError::Corrupt("snapshot phase"))?
        {
            1 => at += 1,
            2 => {
                at += 1;
                let _ = read_fixed::<32>(bytes, &mut at)?;
                let _ = read_fixed::<8>(bytes, &mut at)?;
                let reason = *bytes
                    .get(at)
                    .ok_or(JournalError::Corrupt("snapshot reason"))?;
                reason_from_tag(reason)?;
                at += 1;
            }
            3 => {
                at += 1;
                let _ = read_bytes_ref(bytes, &mut at)?;
            }
            _ => return Err(JournalError::Corrupt("snapshot phase")),
        }
    }
    if at != bytes.len() {
        return Err(JournalError::Corrupt("snapshot trailing bytes"));
    }
    if snapshot_digest(bytes, root_offset)? != root {
        return Err(JournalError::Corrupt("snapshot root"));
    }
    Ok(())
}

fn validate_snapshot_limits(bytes: usize) -> Result<(), JournalError> {
    if bytes > SNAPSHOT_MAX_BYTES {
        return Err(JournalError::Bounds);
    }
    Ok(())
}

pub(super) fn snapshot_header_bytes(base: Option<StoredCheckpoint>) -> usize {
    // tag + format + base flag + last ordinal + root + entry count
    1 + 1
        + 1
        + 8
        + 32
        + 8
        + base.map_or(0, |_| {
            // offset + sequence + chain + record
            8 + 8 + 32 + 32
        })
}

pub(super) fn snapshot_entry_bytes(
    intent_bytes: usize,
    state: &RawSnapshotState,
) -> Result<usize, JournalError> {
    // key + intent length + intent bytes + state tag
    let mut bytes = 32usize
        .checked_add(8)
        .and_then(|bytes| bytes.checked_add(intent_bytes))
        .and_then(|bytes| bytes.checked_add(1))
        .ok_or(JournalError::Bounds)?;
    let state_bytes = match state {
        RawSnapshotState::Prepared => 0,
        RawSnapshotState::Ambiguous { .. } => 32 + 8 + 1,
        RawSnapshotState::Confirmed { receipt } => 8usize
            .checked_add(receipt.len())
            .ok_or(JournalError::Bounds)?,
    };
    bytes = bytes.checked_add(state_bytes).ok_or(JournalError::Bounds)?;
    Ok(bytes)
}

/// Reads only the fixed header through the entry count. This lets a caller's
/// smaller entry budget reject a selected snapshot before `decode_snapshot`
/// allocates its bounded vector.
pub(super) fn snapshot_entry_count(bytes: &[u8]) -> Result<usize, JournalError> {
    validate_snapshot_limits(bytes.len())?;
    let mut at = 0;
    if *bytes.get(at).ok_or(JournalError::Corrupt("snapshot tag"))? != SNAPSHOT_TAG {
        return Err(JournalError::Corrupt("snapshot tag"));
    }
    at += 1;
    if *bytes
        .get(at)
        .ok_or(JournalError::Corrupt("snapshot format"))?
        != SNAPSHOT_FORMAT
    {
        return Err(JournalError::Corrupt("snapshot format"));
    }
    at += 1;
    match *bytes
        .get(at)
        .ok_or(JournalError::Corrupt("snapshot base"))?
    {
        0 => at += 1,
        1 => {
            at += 1;
            let _ = read_fixed::<8>(bytes, &mut at)?;
            let _ = read_fixed::<8>(bytes, &mut at)?;
            let _ = read_fixed::<32>(bytes, &mut at)?;
            let _ = read_fixed::<32>(bytes, &mut at)?;
        }
        _ => return Err(JournalError::Corrupt("snapshot base")),
    }
    let _ = read_fixed::<8>(bytes, &mut at)?;
    let _ = read_fixed::<32>(bytes, &mut at)?;
    let count = usize::try_from(u64::from_be_bytes(read_fixed::<8>(bytes, &mut at)?))
        .map_err(|_| JournalError::Bounds)?;
    if count > SNAPSHOT_MAX_ENTRIES {
        return Err(JournalError::Bounds);
    }
    Ok(count)
}

impl JournalCodec for EffectSnapshotLog {
    type Record = RawEffectSnapshot;

    fn encode(record: &Self::Record, output: &mut Vec<u8>) {
        encode_snapshot(record, output);
    }

    fn decode(bytes: &[u8]) -> Result<Self::Record, JournalError> {
        decode_snapshot(bytes)
    }

    fn validate(bytes: &[u8]) -> Result<(), JournalError> {
        validate_snapshot(bytes)
    }
}

pub(super) fn snapshot_receipt_from_journal(
    receipt: JournalReceipt<EffectSnapshotLog>,
) -> EffectSnapshotReceipt {
    EffectSnapshotReceipt {
        offset: receipt.offset,
        sequence: receipt.sequence,
        chain: *receipt.chain.as_bytes(),
        record: *receipt.record.as_bytes(),
    }
}

pub(super) fn snapshot_checkpoint(
    receipt: EffectSnapshotReceipt,
) -> JournalCheckpoint<EffectSnapshotLog> {
    JournalCheckpoint {
        offset: receipt.offset,
        sequence: receipt.sequence,
        chain: ChainHash::from_bytes(receipt.chain),
        record: crate::schema::RecordId::from_bytes(receipt.record),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unanchored_nonempty_snapshot_is_rejected_before_restore() {
        let snapshot = RawEffectSnapshot {
            base: None,
            last_ordinal: 0,
            root: [0; 32],
            entries: vec![RawSnapshotEntry {
                key: [1; 32],
                intent: vec![2],
                state: RawSnapshotState::Prepared,
            }],
        };
        let mut bytes = Vec::new();
        encode_snapshot(&snapshot, &mut bytes);
        assert!(matches!(
            validate_snapshot(&bytes),
            Err(JournalError::Corrupt("snapshot checkpoint"))
        ));
        assert!(matches!(
            decode_snapshot(&bytes),
            Err(JournalError::Corrupt("snapshot checkpoint"))
        ));
    }
}
