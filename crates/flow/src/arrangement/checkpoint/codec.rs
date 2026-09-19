//! Canonical checkpoint wire codec and immutable node/manifest encodings.

use super::{
    Arrangement, CHECKPOINT_MAGIC, CanonicalValue, CheckpointKeyDecoder, CheckpointValue, Debug,
    DecodedNode, FlowError, Frontier, HistoryRef, MAX_WIRE_COUNT, NODE_BRANCH, NODE_EMPTY,
    NODE_LEAF, NODE_MAGIC, NodeChild, ObjectRef, RowKey, Time, TraceCheckpoint,
};
#[cfg(test)]
use super::{DecodedHistory, DecodedManifest, WorkScope};
use crate::ArrangementKey;

pub(super) fn take_bytes<'a>(bytes: &mut &'a [u8], count: usize) -> Result<&'a [u8], FlowError> {
    if bytes.len() < count {
        return Err(FlowError::InvalidRoot);
    }
    let (head, tail) = bytes.split_at(count);
    *bytes = tail;
    Ok(head)
}

pub(super) fn read_u64_after_tag(bytes: &mut &[u8]) -> Result<u64, FlowError> {
    let _ = take_bytes(bytes, 1)?;
    let encoded = take_bytes(bytes, 8)?;
    Ok(u64::from_be_bytes(
        encoded.try_into().map_err(|_| FlowError::InvalidRoot)?,
    ))
}

pub(super) fn checked_length_after_tag(bytes: &mut &[u8]) -> Result<usize, FlowError> {
    usize::try_from(read_u64_after_tag(bytes)?).map_err(|_| FlowError::Overflow)
}

pub(super) fn push_count(bytes: &mut Vec<u8>, value: usize) -> Result<(), FlowError> {
    bytes.extend_from_slice(
        &u64::try_from(value)
            .map_err(|_| FlowError::Overflow)?
            .to_be_bytes(),
    );
    Ok(())
}

pub(super) fn read_u64(bytes: &mut &[u8]) -> Result<u64, FlowError> {
    Ok(u64::from_be_bytes(
        take_bytes(bytes, 8)?
            .try_into()
            .map_err(|_| FlowError::InvalidRoot)?,
    ))
}

#[cfg(test)]
pub(super) fn read_u16(bytes: &mut &[u8]) -> Result<u16, FlowError> {
    Ok(u16::from_be_bytes(
        take_bytes(bytes, 2)?
            .try_into()
            .map_err(|_| FlowError::InvalidRoot)?,
    ))
}

pub(super) fn read_count(bytes: &mut &[u8]) -> Result<usize, FlowError> {
    usize::try_from(read_u64(bytes)?).map_err(|_| FlowError::Overflow)
}

pub(super) fn checked_count(
    bytes: &[u8],
    count: usize,
    minimum: usize,
) -> Result<usize, FlowError> {
    if count > MAX_WIRE_COUNT || count > bytes.len().saturating_div(minimum.max(1)) {
        return Err(FlowError::InvalidRoot);
    }
    Ok(count)
}

pub(super) fn push_time(bytes: &mut Vec<u8>, time: Time) {
    bytes.extend_from_slice(&time.epoch.0.to_be_bytes());
    bytes.extend_from_slice(&time.iteration.to_be_bytes());
}

#[cfg(test)]
pub(super) fn read_time(bytes: &mut &[u8]) -> Result<Time, FlowError> {
    Ok(Time::new(
        crate::Epoch(read_u64(bytes)?),
        u16::from_be_bytes(
            take_bytes(bytes, 2)?
                .try_into()
                .map_err(|_| FlowError::InvalidRoot)?,
        ),
    ))
}

pub(super) fn push_frontier(bytes: &mut Vec<u8>, frontier: &Frontier) -> Result<(), FlowError> {
    push_count(bytes, frontier.elements().len())?;
    for time in frontier.elements() {
        push_time(bytes, *time);
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn read_frontier(bytes: &mut &[u8]) -> Result<Frontier, FlowError> {
    let count = read_count(bytes)?;
    let count = checked_count(bytes, count, 10)?;
    let mut times = Vec::with_capacity(count);
    for _ in 0..count {
        times.push(read_time(bytes)?);
    }
    Frontier::from_antichain(times)
}

pub(super) fn push_root(bytes: &mut Vec<u8>, root: &[u8; 32]) {
    bytes.extend_from_slice(root);
}

pub(super) fn read_root(bytes: &mut &[u8]) -> Result<[u8; 32], FlowError> {
    take_bytes(bytes, 32)?
        .try_into()
        .map_err(|_| FlowError::InvalidRoot)
}

#[cfg(test)]
pub(super) fn read_length_prefixed_root(bytes: &mut &[u8]) -> Result<[u8; 32], FlowError> {
    if read_count(bytes)? != 32 {
        return Err(FlowError::InvalidRoot);
    }
    read_root(bytes)
}

pub(super) fn push_row_key(bytes: &mut Vec<u8>, key: RowKey) {
    bytes.extend_from_slice(key.relation.as_bytes());
    bytes.extend_from_slice(key.object.as_bytes());
    bytes.extend_from_slice(&key.key.to_be_bytes());
}

pub(super) fn read_row_key<K: CheckpointKeyDecoder>(
    bytes: &mut &[u8],
    key_decoder: &K,
) -> Result<RowKey, FlowError> {
    let relation = read_root(bytes)?;
    let object = read_root(bytes)?;
    let key = read_u64(bytes)?;
    let resolved = key_decoder.decode_key(&relation, &object, key)?;
    if resolved.relation.as_bytes() != &relation
        || resolved.object.as_bytes() != &object
        || resolved.key != key
    {
        return Err(FlowError::InvalidRoot);
    }
    Ok(resolved)
}

pub(super) fn encode_leaf_node<V: CanonicalValue>(
    logical: [u8; 32],
    level: u16,
    entries: &[(ArrangementKey<V>, i64)],
) -> Result<Vec<u8>, FlowError> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(NODE_MAGIC);
    bytes.push(NODE_LEAF);
    bytes.extend_from_slice(&level.to_be_bytes());
    push_root(&mut bytes, &logical);
    push_count(&mut bytes, entries.len())?;
    for (key, support) in entries {
        if *support == 0 {
            return Err(FlowError::InvalidRoot);
        }
        push_row_key(&mut bytes, key.row());
        bytes.extend_from_slice(&support.to_be_bytes());
        key.value().encode_canonical(&mut bytes);
    }
    Ok(bytes)
}

pub(super) fn encode_branch_node<V: CanonicalValue>(
    logical: [u8; 32],
    level: u16,
    children: &[NodeChild<V>],
) -> Result<Vec<u8>, FlowError> {
    if level == 0 || children.is_empty() {
        return Err(FlowError::InvalidRoot);
    }
    let mut bytes = Vec::new();
    bytes.extend_from_slice(NODE_MAGIC);
    bytes.push(NODE_BRANCH);
    bytes.extend_from_slice(&level.to_be_bytes());
    push_root(&mut bytes, &logical);
    push_count(&mut bytes, children.len())?;
    for child in children {
        push_root(&mut bytes, &child.logical);
        push_root(&mut bytes, &child.object);
        bytes.extend_from_slice(&child.row_count.to_be_bytes());
        push_row_key(&mut bytes, child.first.row());
        child.first.value().encode_canonical(&mut bytes);
    }
    Ok(bytes)
}

pub(super) fn decode_node<V: CheckpointValue, K: CheckpointKeyDecoder>(
    mut bytes: &[u8],
    key_decoder: &K,
) -> Result<DecodedNode<V>, FlowError> {
    if !bytes.starts_with(NODE_MAGIC) {
        return Err(FlowError::InvalidRoot);
    }
    bytes = &bytes[NODE_MAGIC.len()..];
    let kind = take_bytes(&mut bytes, 1)?[0];
    let level = u16::from_be_bytes(
        take_bytes(&mut bytes, 2)?
            .try_into()
            .map_err(|_| FlowError::InvalidRoot)?,
    );
    let logical = read_root(&mut bytes)?;
    let count = read_count(&mut bytes)?;
    let (entries, children) = match kind {
        NODE_EMPTY => {
            if level != 0 || count != 0 {
                return Err(FlowError::InvalidRoot);
            }
            (Some(Vec::new()), Vec::new())
        }
        NODE_LEAF => {
            if level != 0 {
                return Err(FlowError::InvalidRoot);
            }
            let count = checked_count(bytes, count, 32 + 8 + 1)?;
            let mut entries = Vec::with_capacity(count);
            for _ in 0..count {
                let key = read_row_key(&mut bytes, key_decoder)?;
                let support = i64::from_be_bytes(
                    take_bytes(&mut bytes, 8)?
                        .try_into()
                        .map_err(|_| FlowError::InvalidRoot)?,
                );
                if support == 0 {
                    return Err(FlowError::InvalidRoot);
                }
                let value = V::decode_checkpoint(&mut bytes)?;
                entries.push((ArrangementKey::new(key, value), support));
            }
            (Some(entries), Vec::new())
        }
        NODE_BRANCH => {
            if level == 0 {
                return Err(FlowError::InvalidRoot);
            }
            let count = checked_count(bytes, count, 64 + 32 + 1)?;
            let mut children = Vec::with_capacity(count);
            for _ in 0..count {
                let logical = read_root(&mut bytes)?;
                let object = read_root(&mut bytes)?;
                let row_count = read_u64(&mut bytes)?;
                let first_key = read_row_key(&mut bytes, key_decoder)?;
                let first_value = V::decode_checkpoint(&mut bytes)?;
                children.push(NodeChild {
                    logical,
                    object,
                    first: ArrangementKey::new(first_key, first_value),
                    row_count,
                });
            }
            (None, children)
        }
        _ => return Err(FlowError::InvalidRoot),
    };
    if !bytes.is_empty() {
        return Err(FlowError::InvalidRoot);
    }
    Ok(DecodedNode {
        logical,
        level,
        entries,
        children,
    })
}

#[cfg(test)]
pub(super) fn decode_manifest(
    mut bytes: &[u8],
    scope: &mut WorkScope,
) -> Result<DecodedManifest, FlowError> {
    if !bytes.starts_with(CHECKPOINT_MAGIC) {
        return Err(FlowError::InvalidRoot);
    }
    scope.charge_bytes(bytes.len())?;
    bytes = &bytes[CHECKPOINT_MAGIC.len()..];
    let layout = read_root(&mut bytes)?;
    let logical_root = read_root(&mut bytes)?;
    let next_batch = read_u64(&mut bytes)?;
    let retained_rows = read_u64(&mut bytes)?;
    let since = read_time(&mut bytes)?;
    let upper = read_frontier(&mut bytes)?;
    let since_frontier = read_frontier(&mut bytes)?;
    let visible = ObjectRef {
        logical: read_root(&mut bytes)?,
        object: read_root(&mut bytes)?,
    };
    let reference_index = ObjectRef {
        logical: read_root(&mut bytes)?,
        object: read_root(&mut bytes)?,
    };
    let reference_index_depth = read_u16(&mut bytes)?;

    let level_count = checked_count(bytes, read_count(&mut bytes)?, 8)?;
    if level_count == 0 {
        return Err(FlowError::InvalidRoot);
    }
    let mut levels = Vec::with_capacity(level_count);
    for _ in 0..level_count {
        let run_count = checked_count(bytes, read_count(&mut bytes)?, 64)?;
        let mut level = Vec::with_capacity(run_count);
        for _ in 0..run_count {
            level.push(ObjectRef {
                logical: read_root(&mut bytes)?,
                object: read_root(&mut bytes)?,
            });
        }
        levels.push(level);
    }

    let history_count = checked_count(bytes, read_count(&mut bytes)?, 8 + 32 * 4)?;
    let mut history = Vec::with_capacity(history_count);
    for _ in 0..history_count {
        let sequence = read_u64(&mut bytes)?;
        let before = read_root(&mut bytes)?;
        let after = read_root(&mut bytes)?;
        let bytes_len = usize::try_from(read_u64(&mut bytes)?).map_err(|_| FlowError::Overflow)?;
        let run = ObjectRef {
            logical: read_root(&mut bytes)?,
            object: read_root(&mut bytes)?,
        };
        history.push(DecodedHistory {
            sequence,
            before,
            after,
            bytes: bytes_len,
            run,
        });
    }
    let max_history = usize::try_from(read_u64(&mut bytes)?).map_err(|_| FlowError::Overflow)?;
    let max_history_rows =
        usize::try_from(read_u64(&mut bytes)?).map_err(|_| FlowError::Overflow)?;
    let max_history_bytes =
        usize::try_from(read_u64(&mut bytes)?).map_err(|_| FlowError::Overflow)?;
    let max_subscription_events =
        usize::try_from(read_u64(&mut bytes)?).map_err(|_| FlowError::Overflow)?;
    let max_subscription_rows =
        usize::try_from(read_u64(&mut bytes)?).map_err(|_| FlowError::Overflow)?;
    if !bytes.is_empty() {
        return Err(FlowError::InvalidRoot);
    }
    Ok(DecodedManifest {
        layout,
        logical_root,
        next_batch,
        retained_rows,
        since,
        upper,
        since_frontier,
        visible,
        reference_index,
        reference_index_depth,
        levels,
        history,
        max_history,
        max_history_rows,
        max_history_bytes,
        max_subscription_events,
        max_subscription_rows,
    })
}

pub(super) fn encode_manifest<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static>(
    arrangement: &Arrangement<V>,
    checkpoint: &TraceCheckpoint<V>,
    visible: ObjectRef,
    reference_index: ObjectRef,
    reference_index_depth: u16,
    levels: &[Vec<ObjectRef>],
    history: &[HistoryRef],
) -> Result<Vec<u8>, FlowError> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(CHECKPOINT_MAGIC);
    push_root(&mut bytes, checkpoint.layout.as_bytes());
    push_root(&mut bytes, checkpoint.logical_root.as_bytes());
    bytes.extend_from_slice(&arrangement.next_batch.to_be_bytes());
    bytes.extend_from_slice(&checkpoint.retained_rows.to_be_bytes());
    push_time(&mut bytes, checkpoint.since);
    push_frontier(&mut bytes, &checkpoint.upper)?;
    push_frontier(&mut bytes, &checkpoint.since_frontier)?;
    push_root(&mut bytes, &visible.logical);
    push_root(&mut bytes, &visible.object);
    push_root(&mut bytes, &reference_index.logical);
    push_root(&mut bytes, &reference_index.object);
    bytes.extend_from_slice(&reference_index_depth.to_be_bytes());

    push_count(&mut bytes, levels.len())?;
    for level in levels {
        push_count(&mut bytes, level.len())?;
        for run in level {
            push_root(&mut bytes, &run.logical);
            push_root(&mut bytes, &run.object);
        }
    }

    // History rows are referenced by immutable run objects. The descriptor
    // carries only their sequence/root/size and never embeds delta payloads.
    push_count(&mut bytes, history.len())?;
    for record in history {
        bytes.extend_from_slice(&record.sequence.to_be_bytes());
        push_root(&mut bytes, &record.before);
        push_root(&mut bytes, &record.after);
        bytes.extend_from_slice(
            &u64::try_from(record.bytes)
                .map_err(|_| FlowError::Overflow)?
                .to_be_bytes(),
        );
        push_root(&mut bytes, &record.run.logical);
        push_root(&mut bytes, &record.run.object);
    }
    bytes.extend_from_slice(
        &u64::try_from(arrangement.max_history)
            .map_err(|_| FlowError::Overflow)?
            .to_be_bytes(),
    );
    bytes.extend_from_slice(
        &u64::try_from(arrangement.max_history_rows)
            .map_err(|_| FlowError::Overflow)?
            .to_be_bytes(),
    );
    bytes.extend_from_slice(
        &u64::try_from(arrangement.max_history_bytes)
            .map_err(|_| FlowError::Overflow)?
            .to_be_bytes(),
    );
    bytes.extend_from_slice(
        &u64::try_from(arrangement.max_subscription_events)
            .map_err(|_| FlowError::Overflow)?
            .to_be_bytes(),
    );
    bytes.extend_from_slice(
        &u64::try_from(arrangement.max_subscription_rows)
            .map_err(|_| FlowError::Overflow)?
            .to_be_bytes(),
    );
    Ok(bytes)
}
