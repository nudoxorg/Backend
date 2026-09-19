//! Stable dispatch record framing codec.

use super::types::{
    AcceptedResultProof, DISPATCH_RECORD_VERSION, DispatchAttemptKey, DispatchCursor,
    DispatchPhase, DispatchRecord, DispatchRecordError, MAX_DISPATCH_CURSOR_BYTES,
    MAX_DISPATCH_PROOF_BYTES, MAX_DISPATCH_REQUEST_BYTES, NotificationCursor, PublicationAck,
    RemoteAttemptIntent, TerminalState, TransferCheckpointRef,
};
use crate::journal::{JournalCodec, JournalDomain, JournalError};

const LENGTH_BYTES: usize = 8;

/// Dispatch journal domain marker.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DispatchLog;

impl JournalDomain for DispatchLog {
    const DOMAIN: u8 = 0x81;
    const TYPE: u16 = 0x0005;
    const VERSION: u8 = 1;
}

impl From<DispatchRecordError> for JournalError {
    fn from(error: DispatchRecordError) -> Self {
        Self::Record(Box::new(error))
    }
}

impl JournalCodec for DispatchLog {
    type Record = DispatchRecord;

    fn encode(record: &Self::Record, output: &mut Vec<u8>) {
        encode_record(record, output);
    }

    fn decode(bytes: &[u8]) -> Result<Self::Record, JournalError> {
        decode_record(bytes).map_err(Into::into)
    }

    fn validate(bytes: &[u8]) -> Result<(), JournalError> {
        decode_record(bytes).map(|_| ()).map_err(Into::into)
    }
}

const fn tag(record: &DispatchRecord) -> u8 {
    match record {
        DispatchRecord::Admitted { .. } => 1,
        DispatchRecord::Transfer { .. } => 2,
        DispatchRecord::Accepted { .. } => 3,
        DispatchRecord::Published { .. } => 4,
        DispatchRecord::PublicationPending { .. } => 5,
        DispatchRecord::Terminal { .. } => 6,
        DispatchRecord::Cursor { .. } => 7,
        DispatchRecord::Fenced { .. } => 8,
        DispatchRecord::PublishedWithCursor { .. } => 9,
    }
}

fn encode_record(record: &DispatchRecord, output: &mut Vec<u8>) {
    output.push(DISPATCH_RECORD_VERSION);
    output.push(tag(record));
    match record {
        DispatchRecord::Admitted { intent } => {
            encode_key(output, intent.key);
            encode_bytes(output, &intent.request);
            output.extend_from_slice(&intent.root);
            output.extend_from_slice(&intent.fence);
            output.extend_from_slice(&intent.owner_epoch.to_be_bytes());
            output.extend_from_slice(&intent.revocation_version.to_be_bytes());
            output.extend_from_slice(&intent.lease_until.to_be_bytes());
            output.extend_from_slice(&intent.deadline.to_be_bytes());
            output.push(intent.phase as u8);
        }
        DispatchRecord::Transfer { key, progress } => {
            encode_key(output, *key);
            output.extend_from_slice(&progress.transfer);
            output.extend_from_slice(&progress.checkpoint);
            output.extend_from_slice(&progress.transferred_bytes.to_be_bytes());
            output.extend_from_slice(&progress.cursor.to_be_bytes());
            output.push(progress.phase as u8);
        }
        DispatchRecord::Accepted { key, proof } => {
            encode_key(output, *key);
            output.extend_from_slice(&proof.output_root);
            output.extend_from_slice(&proof.output_object);
            output.extend_from_slice(&proof.manifest_object);
            encode_bytes(output, &proof.proof);
            encode_bytes(output, &proof.provenance);
            output.extend_from_slice(&proof.accepted_at.to_be_bytes());
        }
        DispatchRecord::Published { key, ack } => {
            encode_key(output, *key);
            output.extend_from_slice(&ack.output_root);
            encode_store_receipt(output, ack.store);
            output.extend_from_slice(&ack.owner_epoch.to_be_bytes());
            output.extend_from_slice(&ack.notification_cursor.to_be_bytes());
        }
        DispatchRecord::PublishedWithCursor { key, ack, cursor } => {
            encode_key(output, *key);
            output.extend_from_slice(&ack.output_root);
            encode_store_receipt(output, ack.store);
            output.extend_from_slice(&ack.owner_epoch.to_be_bytes());
            output.extend_from_slice(&ack.notification_cursor.to_be_bytes());
            output.extend_from_slice(&cursor.waiter.to_be_bytes());
            output.extend_from_slice(&cursor.subscription.to_be_bytes());
            encode_bytes(output, &cursor.cursor);
        }
        DispatchRecord::PublicationPending { key } => encode_key(output, *key),
        DispatchRecord::Terminal { key, terminal } => {
            encode_key(output, *key);
            match terminal {
                TerminalState::Cancelled {
                    reason,
                    notification_cursor,
                } => {
                    output.push(1);
                    output.extend_from_slice(&reason.to_be_bytes());
                    output.extend_from_slice(&[0; 32]);
                    output.extend_from_slice(&notification_cursor.to_be_bytes());
                }
                TerminalState::Fallback {
                    output_root,
                    reason,
                    notification_cursor,
                } => {
                    output.push(2);
                    output.extend_from_slice(&reason.to_be_bytes());
                    output.extend_from_slice(output_root);
                    output.extend_from_slice(&notification_cursor.to_be_bytes());
                }
            }
        }
        DispatchRecord::Cursor { cursor } => {
            encode_key(output, cursor.key);
            output.extend_from_slice(&cursor.position.waiter.to_be_bytes());
            output.extend_from_slice(&cursor.position.subscription.to_be_bytes());
            encode_bytes(output, &cursor.position.cursor);
        }
        DispatchRecord::Fenced {
            key,
            owner_epoch,
            fence,
            reason,
        } => {
            encode_key(output, *key);
            output.extend_from_slice(&owner_epoch.to_be_bytes());
            output.extend_from_slice(fence);
            output.extend_from_slice(&reason.to_be_bytes());
        }
    }
}

fn decode_record(bytes: &[u8]) -> Result<DispatchRecord, DispatchRecordError> {
    let mut reader = Reader::new(bytes);
    if reader.u8()? != DISPATCH_RECORD_VERSION {
        return Err(DispatchRecordError::UnsupportedVersion);
    }
    let record = match reader.u8()? {
        1 => decode_admitted(&mut reader)?,
        2 => decode_transfer(&mut reader)?,
        3 => decode_accepted(&mut reader)?,
        4 => decode_published(&mut reader)?,
        5 => DispatchRecord::PublicationPending { key: reader.key()? },
        6 => decode_terminal(&mut reader)?,
        7 => decode_cursor(&mut reader)?,
        8 => decode_fenced(&mut reader)?,
        9 => decode_published_with_cursor(&mut reader)?,
        _ => return Err(DispatchRecordError::InvalidTag),
    };
    reader.finish()?;
    Ok(record)
}

fn decode_admitted(reader: &mut Reader<'_>) -> Result<DispatchRecord, DispatchRecordError> {
    let key = reader.key()?;
    let request = reader.bytes(MAX_DISPATCH_REQUEST_BYTES)?;
    let root = reader.array()?;
    let fence = reader.array()?;
    if fence == [0; 32] {
        return Err(DispatchRecordError::InvalidIdentifier);
    }
    let owner_epoch = reader.u64()?;
    let revocation_version = reader.u64()?;
    let lease_until = reader.u64()?;
    let deadline = reader.u64()?;
    let phase = DispatchPhase::from_tag(reader.u8()?)?;
    if phase != DispatchPhase::Admitted {
        return Err(DispatchRecordError::IllegalTransition);
    }
    Ok(DispatchRecord::Admitted {
        intent: RemoteAttemptIntent {
            key,
            request,
            root,
            fence,
            owner_epoch,
            revocation_version,
            lease_until,
            deadline,
            phase,
        },
    })
}

fn decode_transfer(reader: &mut Reader<'_>) -> Result<DispatchRecord, DispatchRecordError> {
    let key = reader.key()?;
    let transfer = reader.array()?;
    let checkpoint = reader.array()?;
    let transferred_bytes = reader.u64()?;
    let cursor = reader.u64()?;
    let phase = DispatchPhase::from_tag(reader.u8()?)?;
    let progress =
        TransferCheckpointRef::new(transfer, checkpoint, transferred_bytes, cursor, phase)?;
    Ok(DispatchRecord::Transfer { key, progress })
}

fn decode_accepted(reader: &mut Reader<'_>) -> Result<DispatchRecord, DispatchRecordError> {
    let key = reader.key()?;
    let output_root = reader.array()?;
    let output_object = reader.array()?;
    let manifest_object = reader.array()?;
    let proof = reader.bytes(MAX_DISPATCH_PROOF_BYTES)?;
    let provenance = reader.bytes(MAX_DISPATCH_PROOF_BYTES)?;
    let accepted_at = reader.u64()?;
    Ok(DispatchRecord::Accepted {
        key,
        proof: AcceptedResultProof {
            output_root,
            output_object,
            manifest_object,
            proof,
            provenance,
            accepted_at,
        },
    })
}

fn decode_published(reader: &mut Reader<'_>) -> Result<DispatchRecord, DispatchRecordError> {
    let key = reader.key()?;
    let output_root = reader.array()?;
    let store = decode_store_receipt(reader)?;
    let owner_epoch = reader.u64()?;
    let notification_cursor = reader.u64()?;
    Ok(DispatchRecord::Published {
        key,
        ack: PublicationAck {
            output_root,
            store,
            owner_epoch,
            notification_cursor,
        },
    })
}

fn decode_terminal(reader: &mut Reader<'_>) -> Result<DispatchRecord, DispatchRecordError> {
    let key = reader.key()?;
    let kind = reader.u8()?;
    let reason = reader.u16()?;
    let output_root = reader.array()?;
    let notification_cursor = reader.u64()?;
    let terminal = match kind {
        1 => TerminalState::Cancelled {
            reason,
            notification_cursor,
        },
        2 => TerminalState::Fallback {
            output_root,
            reason,
            notification_cursor,
        },
        _ => return Err(DispatchRecordError::InvalidTag),
    };
    Ok(DispatchRecord::Terminal { key, terminal })
}

fn decode_cursor(reader: &mut Reader<'_>) -> Result<DispatchRecord, DispatchRecordError> {
    let key = reader.key()?;
    let waiter = reader.u64()?;
    let subscription = reader.u64()?;
    let cursor = reader.bytes(MAX_DISPATCH_CURSOR_BYTES)?;
    Ok(DispatchRecord::Cursor {
        cursor: DispatchCursor {
            key,
            position: NotificationCursor {
                waiter,
                subscription,
                cursor,
            },
        },
    })
}

fn decode_fenced(reader: &mut Reader<'_>) -> Result<DispatchRecord, DispatchRecordError> {
    let key = reader.key()?;
    let owner_epoch = reader.u64()?;
    let fence = reader.array()?;
    if fence == [0; 32] {
        return Err(DispatchRecordError::InvalidIdentifier);
    }
    let reason = reader.u16()?;
    Ok(DispatchRecord::Fenced {
        key,
        owner_epoch,
        fence,
        reason,
    })
}

fn decode_published_with_cursor(
    reader: &mut Reader<'_>,
) -> Result<DispatchRecord, DispatchRecordError> {
    let key = reader.key()?;
    let output_root = reader.array()?;
    let store = decode_store_receipt(reader)?;
    let owner_epoch = reader.u64()?;
    let notification_cursor = reader.u64()?;
    let waiter = reader.u64()?;
    let subscription = reader.u64()?;
    let cursor = reader.bytes(MAX_DISPATCH_CURSOR_BYTES)?;
    Ok(DispatchRecord::PublishedWithCursor {
        key,
        ack: PublicationAck {
            output_root,
            store,
            owner_epoch,
            notification_cursor,
        },
        cursor: NotificationCursor {
            waiter,
            subscription,
            cursor,
        },
    })
}

fn encode_store_receipt(output: &mut Vec<u8>, receipt: super::types::StorePublicationReceipt) {
    output.extend_from_slice(&receipt.transaction());
    output.extend_from_slice(&receipt.selected_sequence().to_be_bytes());
    output.extend_from_slice(&receipt.target());
    output.extend_from_slice(&receipt.workspace_root());
}

fn decode_store_receipt(
    reader: &mut Reader<'_>,
) -> Result<super::types::StorePublicationReceipt, DispatchRecordError> {
    super::types::StorePublicationReceipt::from_parts(
        reader.array()?,
        reader.u64()?,
        reader.array()?,
        reader.array()?,
    )
}

fn encode_key(output: &mut Vec<u8>, key: DispatchAttemptKey) {
    output.extend_from_slice(&key.work_key);
    output.extend_from_slice(&key.attempt.to_be_bytes());
}

fn encode_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    output.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    output.extend_from_slice(bytes);
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], DispatchRecordError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(DispatchRecordError::Bounds)?;
        let bytes = self
            .bytes
            .get(self.position..end)
            .ok_or(DispatchRecordError::Truncated)?;
        self.position = end;
        Ok(bytes)
    }

    fn u8(&mut self) -> Result<u8, DispatchRecordError> {
        Ok(*self
            .take(1)?
            .first()
            .ok_or(DispatchRecordError::Truncated)?)
    }

    fn u16(&mut self) -> Result<u16, DispatchRecordError> {
        Ok(u16::from_be_bytes(
            self.take(2)?
                .try_into()
                .map_err(|_| DispatchRecordError::Truncated)?,
        ))
    }

    fn u64(&mut self) -> Result<u64, DispatchRecordError> {
        Ok(u64::from_be_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| DispatchRecordError::Truncated)?,
        ))
    }

    fn array(&mut self) -> Result<[u8; 32], DispatchRecordError> {
        self.take(32)?
            .try_into()
            .map_err(|_| DispatchRecordError::Truncated)
    }

    fn key(&mut self) -> Result<DispatchAttemptKey, DispatchRecordError> {
        DispatchAttemptKey::new(self.array()?, self.u64()?)
    }

    fn bytes(&mut self, maximum: usize) -> Result<Box<[u8]>, DispatchRecordError> {
        let length = usize::try_from(u64::from_be_bytes(
            self.take(LENGTH_BYTES)?
                .try_into()
                .map_err(|_| DispatchRecordError::Truncated)?,
        ))
        .map_err(|_| DispatchRecordError::Bounds)?;
        if length > maximum {
            return Err(DispatchRecordError::Bounds);
        }
        Ok(self.take(length)?.into())
    }

    fn finish(&self) -> Result<(), DispatchRecordError> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(DispatchRecordError::InvalidTag)
        }
    }
}
