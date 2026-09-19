//! Compact diagnostic workspace journal records and codecs.

use super::transition::{PersistedTransition, TransactionId};
use crate::journal::{JournalCodec, JournalError, JournalReceipt};
use crate::schema::WorkspaceLog;

const MAX_RECORD_BYTES: usize = 64 * 1024 * 1024;

/// Durable workspace journal record.  Bytes are canonical persistence
/// descriptors; the owner admits them through `WorkspaceModel::admit_persisted`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkspaceRecord {
    /// Immutable closure and transition have been prepared.
    Prepared {
        /// Client request identity.
        request: [u8; 32],
        /// Deterministic transaction identity.
        transaction: TransactionId,
        /// Canonical target manifest.
        manifest: Vec<u8>,
        /// Canonical workspace delta.
        delta: Vec<u8>,
        /// Canonical commit.
        commit: Vec<u8>,
        /// Canonical store closure.
        closure: Vec<u8>,
    },
    /// The prepared transition has an ordered selected sequence. The record is
    /// a compact link to the exact Prepared frame; it never repeats the large
    /// manifest, delta, commit, or closure payload.
    Select {
        /// Deterministic transaction identity.
        transaction: TransactionId,
        /// Visible workspace sequence.
        sequence: u64,
        /// Sequence of the exact Prepared frame.
        prepared_sequence: u64,
        /// Byte offset of the exact Prepared frame.
        prepared_offset: u64,
        /// Hash-chain identity of the Prepared frame.
        prepared_chain: [u8; 32],
        /// Content identity of the Prepared payload.
        prepared_record: [u8; 32],
    },
    /// The selected head was made visible.
    Published {
        /// Deterministic transaction identity.
        transaction: TransactionId,
        /// Visible workspace sequence.
        sequence: u64,
    },
}

impl JournalCodec for WorkspaceLog {
    type Record = WorkspaceRecord;

    fn encode(record: &Self::Record, output: &mut Vec<u8>) {
        match record {
            WorkspaceRecord::Prepared {
                request,
                transaction,
                manifest,
                delta,
                commit,
                closure,
            } => {
                output.push(1);
                output.extend_from_slice(request);
                output.extend_from_slice(&transaction.as_bytes());
                put_bytes(output, manifest);
                put_bytes(output, delta);
                put_bytes(output, commit);
                put_bytes(output, closure);
            }
            WorkspaceRecord::Select {
                transaction,
                sequence,
                prepared_sequence,
                prepared_offset,
                prepared_chain,
                prepared_record,
            } => {
                output.push(2);
                output.extend_from_slice(&transaction.as_bytes());
                output.extend_from_slice(&sequence.to_be_bytes());
                output.extend_from_slice(&prepared_sequence.to_be_bytes());
                output.extend_from_slice(&prepared_offset.to_be_bytes());
                output.extend_from_slice(prepared_chain);
                output.extend_from_slice(prepared_record);
            }
            WorkspaceRecord::Published {
                transaction,
                sequence,
            } => {
                output.push(3);
                output.extend_from_slice(&transaction.as_bytes());
                output.extend_from_slice(&sequence.to_be_bytes());
            }
        }
    }

    fn decode(bytes: &[u8]) -> Result<Self::Record, JournalError> {
        let tag = *bytes.first().ok_or(JournalError::Corrupt("record tag"))?;
        let mut at = 1usize;
        let request = if tag == 1 {
            read_fixed::<32>(bytes, &mut at)?
        } else {
            [0; 32]
        };
        let transaction = TransactionId::from_bytes(read_fixed::<32>(bytes, &mut at)?);
        let sequence = if tag == 2 || tag == 3 {
            u64::from_be_bytes(read_fixed::<8>(bytes, &mut at)?)
        } else {
            0
        };
        let manifest = if tag == 1 {
            read_bytes(bytes, &mut at)?
        } else {
            Vec::new()
        };
        let delta = if tag == 1 {
            read_bytes(bytes, &mut at)?
        } else {
            Vec::new()
        };
        let commit = if tag == 1 {
            read_bytes(bytes, &mut at)?
        } else {
            Vec::new()
        };
        let closure = if tag == 1 {
            read_bytes(bytes, &mut at)?
        } else {
            Vec::new()
        };
        let prepared_sequence = if tag == 2 {
            u64::from_be_bytes(read_fixed::<8>(bytes, &mut at)?)
        } else {
            0
        };
        let prepared_offset = if tag == 2 {
            u64::from_be_bytes(read_fixed::<8>(bytes, &mut at)?)
        } else {
            0
        };
        let prepared_chain = if tag == 2 {
            read_fixed::<32>(bytes, &mut at)?
        } else {
            [0; 32]
        };
        let prepared_record = if tag == 2 {
            read_fixed::<32>(bytes, &mut at)?
        } else {
            [0; 32]
        };
        if at != bytes.len() {
            return Err(JournalError::Corrupt("record trailing bytes"));
        }
        Ok(match tag {
            1 => WorkspaceRecord::Prepared {
                request,
                transaction,
                manifest,
                delta,
                commit,
                closure,
            },
            2 => WorkspaceRecord::Select {
                transaction,
                sequence,
                prepared_sequence,
                prepared_offset,
                prepared_chain,
                prepared_record,
            },
            3 => WorkspaceRecord::Published {
                transaction,
                sequence,
            },
            _ => return Err(JournalError::Corrupt("record tag")),
        })
    }
}

/// Encodes the legacy full Prepared diagnostic record. New recovery never
/// trusts this payload; the store closure is authoritative, but retaining the
/// record keeps diagnostics and explicit forensic scans self contained.
pub(crate) fn encode_prepared(output: &mut Vec<u8>, persisted: &PersistedTransition) {
    output.push(1);
    output.extend_from_slice(&persisted.request());
    output.extend_from_slice(&persisted.transaction().as_bytes());
    put_bytes(output, persisted.manifest_bytes());
    put_bytes(output, persisted.delta_bytes());
    put_bytes(output, persisted.commit_bytes());
    put_bytes(output, persisted.closure_bytes());
}

/// Encodes a compact Select diagnostic link.
pub(crate) fn encode_select(
    output: &mut Vec<u8>,
    transaction: TransactionId,
    sequence: u64,
    prepared: JournalReceipt<WorkspaceLog>,
) {
    output.push(2);
    output.extend_from_slice(&transaction.as_bytes());
    output.extend_from_slice(&sequence.to_be_bytes());
    output.extend_from_slice(&prepared.sequence.to_be_bytes());
    output.extend_from_slice(&prepared.offset.to_be_bytes());
    output.extend_from_slice(prepared.chain.as_bytes());
    output.extend_from_slice(prepared.record.as_bytes());
}

/// Encodes a compact Published diagnostic marker.
pub(crate) fn encode_published(output: &mut Vec<u8>, transaction: TransactionId, sequence: u64) {
    output.push(3);
    output.extend_from_slice(&transaction.as_bytes());
    output.extend_from_slice(&sequence.to_be_bytes());
}

fn put_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    output.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    output.extend_from_slice(bytes);
}

fn read_fixed<const N: usize>(bytes: &[u8], at: &mut usize) -> Result<[u8; N], JournalError> {
    let end = at.checked_add(N).ok_or(JournalError::Bounds)?;
    let value = bytes
        .get(*at..end)
        .ok_or(JournalError::Corrupt("fixed field"))?;
    *at = end;
    value
        .try_into()
        .map_err(|_| JournalError::Corrupt("fixed field"))
}

fn read_bytes(bytes: &[u8], at: &mut usize) -> Result<Vec<u8>, JournalError> {
    let length = usize::try_from(u64::from_be_bytes(read_fixed::<8>(bytes, at)?))
        .map_err(|_| JournalError::Bounds)?;
    if length > MAX_RECORD_BYTES {
        return Err(JournalError::Bounds);
    }
    let end = at.checked_add(length).ok_or(JournalError::Bounds)?;
    let value = bytes
        .get(*at..end)
        .ok_or(JournalError::Corrupt("bytes"))?
        .to_vec();
    *at = end;
    Ok(value)
}
