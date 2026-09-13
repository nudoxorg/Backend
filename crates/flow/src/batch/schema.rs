//! Canonical batch and trace identity helpers.

use crate::types::RawDigest;
use crate::{CanonicalValue, Delta, FlowError, RowKey};
use backend_version::{ObjectVersion, Schema};
use std::cmp::Ordering;

/// Schema for a signed batch content identity.
pub struct BatchSchema;
impl Schema for BatchSchema {
    const DOMAIN: u8 = 0x66;
    const TYPE: u16 = 9;
    type Value = Vec<u8>;

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Schema for one immutable physical trace run identity.
pub struct RunSchema;
impl Schema for RunSchema {
    const DOMAIN: u8 = 0x66;
    const TYPE: u16 = 10;
    type Value = Vec<u8>;

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Typed identity of one signed batch's canonical bytes.
pub type BatchRoot = ObjectVersion<BatchSchema>;
/// Typed identity of one immutable physical run.
pub type RunRoot = ObjectVersion<RunSchema>;

fn encode_tagged_bytes(tag: u8, bytes: &[u8], out: &mut Vec<u8>) {
    out.push(tag);
    append_len_prefixed(out, bytes);
}

fn canonical_len(value: usize) -> u64 {
    u64::try_from(value).map_or(u64::MAX, |value| value)
}

impl CanonicalValue for i64 {
    fn encode_canonical(&self, out: &mut Vec<u8>) {
        out.push(b'i');
        out.extend_from_slice(&self.to_be_bytes());
    }
    fn encode_ordered(&self, out: &mut Vec<u8>) {
        out.push(b'i');
        out.extend_from_slice(&(self.cast_unsigned() ^ (1 << 63)).to_be_bytes());
    }
}

impl CanonicalValue for u64 {
    fn encode_canonical(&self, out: &mut Vec<u8>) {
        out.push(b'U');
        out.extend_from_slice(&self.to_be_bytes());
    }
    fn encode_ordered(&self, out: &mut Vec<u8>) {
        self.encode_canonical(out);
    }
}

impl CanonicalValue for i32 {
    fn encode_canonical(&self, out: &mut Vec<u8>) {
        out.push(b'j');
        out.extend_from_slice(&self.to_be_bytes());
    }
    fn encode_ordered(&self, out: &mut Vec<u8>) {
        out.push(b'j');
        out.extend_from_slice(&(self.cast_unsigned() ^ (1 << 31)).to_be_bytes());
    }
}

impl CanonicalValue for u32 {
    fn encode_canonical(&self, out: &mut Vec<u8>) {
        out.push(b'J');
        out.extend_from_slice(&self.to_be_bytes());
    }
    fn encode_ordered(&self, out: &mut Vec<u8>) {
        self.encode_canonical(out);
    }
}

impl CanonicalValue for i16 {
    fn encode_canonical(&self, out: &mut Vec<u8>) {
        out.push(b'h');
        out.extend_from_slice(&self.to_be_bytes());
    }
    fn encode_ordered(&self, out: &mut Vec<u8>) {
        out.push(b'h');
        out.extend_from_slice(&(self.cast_unsigned() ^ (1 << 15)).to_be_bytes());
    }
}

impl CanonicalValue for u16 {
    fn encode_canonical(&self, out: &mut Vec<u8>) {
        out.push(b'H');
        out.extend_from_slice(&self.to_be_bytes());
    }
    fn encode_ordered(&self, out: &mut Vec<u8>) {
        self.encode_canonical(out);
    }
}

impl CanonicalValue for i8 {
    fn encode_canonical(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&[b'g', self.cast_unsigned()]);
    }
    fn encode_ordered(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&[b'g', (self.cast_unsigned() ^ 0x80)]);
    }
}

impl CanonicalValue for u8 {
    fn encode_canonical(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&[b'G', *self]);
    }
    fn encode_ordered(&self, out: &mut Vec<u8>) {
        self.encode_canonical(out);
    }
}

impl CanonicalValue for usize {
    fn encode_canonical(&self, out: &mut Vec<u8>) {
        out.push(b'Z');
        out.extend_from_slice(&canonical_len(*self).to_be_bytes());
    }
    fn encode_ordered(&self, out: &mut Vec<u8>) {
        self.encode_canonical(out);
    }
}

impl CanonicalValue for bool {
    fn encode_canonical(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&[b'B', u8::from(*self)]);
    }
    fn encode_ordered(&self, out: &mut Vec<u8>) {
        self.encode_canonical(out);
    }
}

impl CanonicalValue for String {
    fn encode_canonical(&self, out: &mut Vec<u8>) {
        encode_tagged_bytes(b'S', self.as_bytes(), out);
    }
    fn encode_ordered(&self, out: &mut Vec<u8>) {
        encode_ordered_bytes(b'S', self.as_bytes(), out);
    }
    fn canonical_len(&self) -> usize {
        1 + 8 + self.len()
    }
}

impl CanonicalValue for Vec<u8> {
    fn encode_canonical(&self, out: &mut Vec<u8>) {
        encode_tagged_bytes(b'V', self, out);
    }
    fn encode_ordered(&self, out: &mut Vec<u8>) {
        encode_ordered_bytes(b'V', self, out);
    }
    fn canonical_len(&self) -> usize {
        1 + 8 + self.len()
    }
}

impl<const N: usize> CanonicalValue for [u8; N] {
    fn encode_canonical(&self, out: &mut Vec<u8>) {
        encode_tagged_bytes(b'A', self, out);
    }
    fn encode_ordered(&self, out: &mut Vec<u8>) {
        encode_ordered_bytes(b'A', self, out);
    }
}

impl<T: CanonicalValue> CanonicalValue for Option<T> {
    fn encode_canonical(&self, out: &mut Vec<u8>) {
        match self {
            Some(value) => {
                out.push(1);
                value.encode_canonical(out);
            }
            None => out.push(0),
        }
    }
    fn encode_ordered(&self, out: &mut Vec<u8>) {
        out.push(b'O');
        match self {
            None => out.push(0),
            Some(value) => {
                out.push(1);
                let mut value_bytes = Vec::new();
                value.encode_ordered(&mut value_bytes);
                encode_ordered_bytes(b'v', &value_bytes, out);
            }
        }
    }
}

impl<A: CanonicalValue, B: CanonicalValue> CanonicalValue for (A, B) {
    fn encode_canonical(&self, out: &mut Vec<u8>) {
        out.push(b'T');
        self.0.encode_canonical(out);
        self.1.encode_canonical(out);
    }
    fn encode_ordered(&self, out: &mut Vec<u8>) {
        out.push(b'T');
        let mut first = Vec::new();
        self.0.encode_ordered(&mut first);
        encode_ordered_bytes(b'a', &first, out);
        let mut second = Vec::new();
        self.1.encode_ordered(&mut second);
        encode_ordered_bytes(b'b', &second, out);
    }
}

pub(crate) fn append_len_prefixed(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&canonical_len(bytes.len()).to_be_bytes());
    out.extend_from_slice(bytes);
}

// A zero byte terminates the sequence; nonzero bytes retain their natural
// order and zero is escaped as 0xff. Thus a shorter byte string sorts before
// its extension while every byte sequence remains injective.
fn encode_ordered_bytes(tag: u8, bytes: &[u8], out: &mut Vec<u8>) {
    out.push(tag);
    for byte in bytes {
        if *byte == 0 {
            out.extend_from_slice(&[0, 0xff]);
        } else {
            out.push(*byte);
        }
    }
    out.extend_from_slice(&[0, 0]);
}

pub(crate) fn canonical_bytes<V: CanonicalValue>(
    deltas: &[Delta<V>],
    parent: RawDigest,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"flow.batch.v2\0");
    out.extend_from_slice(&parent);
    out.extend_from_slice(&canonical_len(deltas.len()).to_be_bytes());
    for row in deltas {
        append_len_prefixed(&mut out, row.key.relation.as_bytes());
        append_len_prefixed(&mut out, row.key.object.as_bytes());
        out.extend_from_slice(&row.key.key.to_be_bytes());
        out.extend_from_slice(&row.time.epoch.0.to_be_bytes());
        out.extend_from_slice(&row.time.iteration.to_be_bytes());
        out.extend_from_slice(&row.diff.value().to_be_bytes());
        row.value.encode_canonical(&mut out);
    }
    out
}

pub(crate) fn batch_root<V: CanonicalValue>(deltas: &[Delta<V>], parent: RawDigest) -> BatchRoot {
    let bytes = canonical_bytes(deltas, parent);
    ObjectVersion::from_value(&bytes)
}

pub(crate) fn run_root<V: CanonicalValue>(deltas: &[Delta<V>]) -> RunRoot {
    let bytes = canonical_bytes(deltas, [0; 32]);
    ObjectVersion::from_value(&bytes)
}

pub(crate) fn row_cmp<V: Ord>(left: &Delta<V>, right: &Delta<V>) -> Ordering {
    left.key
        .cmp(&right.key)
        .then(left.value.cmp(&right.value))
        .then(left.time.cmp(&right.time))
}

pub(crate) fn lower_bound<V: Ord>(rows: &[Delta<V>], key: &RowKey) -> usize {
    let mut low = 0;
    let mut high = rows.len();
    while low < high {
        let middle = low + (high - low) / 2;
        if rows[middle].key < *key {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    low
}

pub(crate) fn upper_bound<V: Ord>(rows: &[Delta<V>], key: &RowKey) -> usize {
    let mut low = 0;
    let mut high = rows.len();
    while low < high {
        let middle = low + (high - low) / 2;
        if rows[middle].key <= *key {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    low
}

pub(crate) fn consolidate_rows<V: Ord>(
    mut rows: Vec<Delta<V>>,
) -> Result<Vec<Delta<V>>, FlowError> {
    if rows.iter().any(Delta::is_zero) {
        return Err(FlowError::ZeroDiff);
    }
    // Microbatches and runs are normally already ordered.  Checking that
    // invariant is linear and avoids repeatedly sorting the same rows while
    // an update crosses the batch, join, and arrangement boundaries.
    if rows
        .windows(2)
        .any(|window| row_cmp(&window[0], &window[1]) == Ordering::Greater)
    {
        rows.sort_unstable_by(row_cmp);
    }
    // Compact in place so the immutable owner can take this allocation
    // directly.  The write cursor only swaps already-processed rows; the
    // unread suffix remains initialized and no second V column is created.
    let mut write = 0usize;
    for read in 0..rows.len() {
        let duplicate = write > 0
            && rows[write - 1].key == rows[read].key
            && rows[write - 1].time == rows[read].time
            && rows[write - 1].value == rows[read].value;
        if duplicate {
            let next = rows[write - 1].diff.checked_add(rows[read].diff)?;
            if let Some(diff) = next {
                rows[write - 1].diff = diff;
            } else {
                write -= 1;
            }
            continue;
        }
        if write != read {
            rows.swap(write, read);
        }
        write += 1;
    }
    rows.truncate(write);
    Ok(rows)
}
