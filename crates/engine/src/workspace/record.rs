//! Compact diagnostic workspace journal records and codecs.

use super::transition::{PersistedTransition, TransactionId};
use crate::journal::{JournalCodec, JournalError, JournalReceipt};
use crate::schema::WorkspaceLog;

const MAX_RECORD_BYTES: usize = 64 * 1024 * 1024;

/// Diagnostic record about workspace publication. The authenticated store
/// closure, rather than this history, is authoritative for owner recovery.
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
    /// Bounded diagnostic summary for a prepared transition, replacing the
    /// full payload in new writes. This is a tag-4 extension to the version-1
    /// workspace-record grammar: a reader that only knows tags 1–3 rejects it.
    /// The owner recovers the authenticated store head before opening this
    /// diagnostic log; older readers quarantine the unsupported journal and
    /// retain its original bytes under the corrupt-journal name. Downgrades
    /// therefore preserve selected state but roll live diagnostics back.
    ///
    /// Store-owned closure recovery remains authoritative. Each length and
    /// digest identifies the bytes retained by that store closure without
    /// copying those bytes into the diagnostic journal.
    PreparedSummary {
        /// Client request identity.
        request: [u8; 32],
        /// Deterministic transaction identity.
        transaction: TransactionId,
        /// Length and digest of the canonical target manifest.
        manifest_bytes: u64,
        /// BLAKE3 digest of the canonical target manifest.
        manifest_digest: [u8; 32],
        /// Length and digest of the canonical workspace delta.
        delta_bytes: u64,
        /// BLAKE3 digest of the canonical workspace delta.
        delta_digest: [u8; 32],
        /// Length and digest of the canonical commit.
        commit_bytes: u64,
        /// BLAKE3 digest of the canonical commit.
        commit_digest: [u8; 32],
        /// Length and digest of the canonical store closure.
        closure_bytes: u64,
        /// BLAKE3 digest of the canonical store closure.
        closure_digest: [u8; 32],
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
            WorkspaceRecord::PreparedSummary {
                request,
                transaction,
                manifest_bytes,
                manifest_digest,
                delta_bytes,
                delta_digest,
                commit_bytes,
                commit_digest,
                closure_bytes,
                closure_digest,
            } => {
                output.push(4);
                output.extend_from_slice(request);
                output.extend_from_slice(&transaction.as_bytes());
                output.extend_from_slice(&manifest_bytes.to_be_bytes());
                output.extend_from_slice(manifest_digest);
                output.extend_from_slice(&delta_bytes.to_be_bytes());
                output.extend_from_slice(delta_digest);
                output.extend_from_slice(&commit_bytes.to_be_bytes());
                output.extend_from_slice(commit_digest);
                output.extend_from_slice(&closure_bytes.to_be_bytes());
                output.extend_from_slice(closure_digest);
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
        // Record grammar revision 2 adds tag 4. Envelope framing and tags
        // 1–3 remain byte-for-byte unchanged; an old closed reader rejects 4.
        if tag == 4 {
            let record = WorkspaceRecord::PreparedSummary {
                request: read_fixed::<32>(bytes, &mut at)?,
                transaction: TransactionId::from_bytes(read_fixed::<32>(bytes, &mut at)?),
                manifest_bytes: read_summary_length(bytes, &mut at)?,
                manifest_digest: read_fixed::<32>(bytes, &mut at)?,
                delta_bytes: read_summary_length(bytes, &mut at)?,
                delta_digest: read_fixed::<32>(bytes, &mut at)?,
                commit_bytes: read_summary_length(bytes, &mut at)?,
                commit_digest: read_fixed::<32>(bytes, &mut at)?,
                closure_bytes: read_summary_length(bytes, &mut at)?,
                closure_digest: read_fixed::<32>(bytes, &mut at)?,
            };
            if at != bytes.len() {
                return Err(JournalError::Corrupt("record trailing bytes"));
            }
            return Ok(record);
        }
        decode_legacy(bytes)
    }
}

// The previous closed record decoder is retained verbatim for tags 1–3.
// Tests use this exact decoder to exercise a rollback reader, rather than
// approximating the old failure with a new tag-only allowlist.
fn decode_legacy(bytes: &[u8]) -> Result<WorkspaceRecord, JournalError> {
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

/// Encodes only identities of the four store-retained Prepared components.
/// Each component is admitted independently under 64 MiB; concatenating all
/// four could exceed the journal's unchanged 64 MiB frame bound. Store-head
/// recovery reads the authenticated closure, never this diagnostic summary.
pub(crate) fn encode_prepared(output: &mut Vec<u8>, persisted: &PersistedTransition) {
    output.push(4);
    output.extend_from_slice(&persisted.request());
    output.extend_from_slice(&persisted.transaction().as_bytes());
    put_summary_part(output, persisted.manifest_bytes());
    put_summary_part(output, persisted.delta_bytes());
    put_summary_part(output, persisted.commit_bytes());
    put_summary_part(output, persisted.closure_bytes());
}

fn put_summary_part(output: &mut Vec<u8>, bytes: &[u8]) {
    output.extend_from_slice(&u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    output.extend_from_slice(blake3::hash(bytes).as_bytes());
}

fn read_summary_length(bytes: &[u8], at: &mut usize) -> Result<u64, JournalError> {
    let length = u64::from_be_bytes(read_fixed::<8>(bytes, at)?);
    if length > u64::try_from(MAX_RECORD_BYTES).unwrap_or(u64::MAX) {
        return Err(JournalError::Bounds);
    }
    Ok(length)
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

#[cfg(test)]
pub(super) mod tests {
    use super::{JournalCodec, WorkspaceLog, WorkspaceRecord};
    use crate::journal::{JournalDomain, JournalError};
    use crate::workspace::TransactionId;

    /// Models the closed tag set in the previous workspace-record reader.
    /// Its envelope/domain version is deliberately unchanged, as in v1.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(in crate::workspace) struct LegacyWorkspaceLog;

    impl JournalDomain for LegacyWorkspaceLog {
        const DOMAIN: u8 = <WorkspaceLog as JournalDomain>::DOMAIN;
        const TYPE: u16 = <WorkspaceLog as JournalDomain>::TYPE;
        const VERSION: u8 = <WorkspaceLog as JournalDomain>::VERSION;
    }

    impl JournalCodec for LegacyWorkspaceLog {
        type Record = WorkspaceRecord;

        fn encode(record: &Self::Record, output: &mut Vec<u8>) {
            WorkspaceLog::encode(record, output);
        }

        fn decode(bytes: &[u8]) -> Result<Self::Record, JournalError> {
            super::decode_legacy(bytes)
        }
    }

    #[test]
    fn v1_reader_rejects_tag4_instead_of_misreading_or_accepting_it() {
        let value = WorkspaceRecord::PreparedSummary {
            request: [1; 32],
            transaction: TransactionId::from_bytes([2; 32]),
            manifest_bytes: 0,
            manifest_digest: [3; 32],
            delta_bytes: 0,
            delta_digest: [4; 32],
            commit_bytes: 0,
            commit_digest: [5; 32],
            closure_bytes: 0,
            closure_digest: [6; 32],
        };
        let mut encoded = Vec::new();
        WorkspaceLog::encode(&value, &mut encoded);
        assert_eq!(encoded.first(), Some(&4));
        assert!(matches!(
            LegacyWorkspaceLog::decode(&encoded),
            Err(JournalError::Corrupt("record trailing bytes"))
        ));
    }

    #[test]
    fn prepared_summary_is_bounded_and_round_trips_component_identity() {
        let large = 64_u64 * 1024 * 1024;
        let value = WorkspaceRecord::PreparedSummary {
            request: [1; 32],
            transaction: TransactionId::from_bytes([2; 32]),
            manifest_bytes: large,
            manifest_digest: [3; 32],
            delta_bytes: large,
            delta_digest: [4; 32],
            commit_bytes: large,
            commit_digest: [5; 32],
            closure_bytes: large,
            closure_digest: [6; 32],
        };
        let mut encoded = Vec::new();
        WorkspaceLog::encode(&value, &mut encoded);
        assert_eq!(encoded.len(), 225);
        assert!(manifest_total(&value) > 64 * 1024 * 1024);
        assert_eq!(WorkspaceLog::decode(&encoded).unwrap(), value);
        assert!(WorkspaceLog::decode(&encoded[..encoded.len() - 1]).is_err());
        let mut trailing = encoded.clone();
        trailing.push(0);
        assert!(WorkspaceLog::decode(&trailing).is_err());
        encoded[65..73].copy_from_slice(&(large + 1).to_be_bytes());
        assert!(matches!(
            WorkspaceLog::decode(&encoded),
            Err(JournalError::Bounds)
        ));
    }

    fn manifest_total(record: &WorkspaceRecord) -> u64 {
        let WorkspaceRecord::PreparedSummary {
            manifest_bytes,
            delta_bytes,
            commit_bytes,
            closure_bytes,
            ..
        } = record
        else {
            panic!("summary expected")
        };
        manifest_bytes + delta_bytes + commit_bytes + closure_bytes
    }
}
