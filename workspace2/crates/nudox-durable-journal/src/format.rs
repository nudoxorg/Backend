use core::mem::size_of;

use blake3::Hasher;
use nudox_workflow::{WORKFLOW_RECORD_BYTES, WorkflowRecord};
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout,
    byteorder::{LittleEndian, U16, U64},
};

use crate::{FrameSequence, HeaderError, JournalOffset};

const MAGIC: [u8; 8] = *b"NUDXJNL\0";
const PHYSICAL_VERSION: u16 = 1;
const CHECKSUM_BYTES: usize = 16;
const HEADER_DOMAIN: &[u8] = b"nudox.journal.header.v1\0";
const FRAME_DOMAIN: &[u8] = b"nudox.journal.frame.v1\0";

#[repr(C)]
#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout)]
pub(crate) struct HeaderRecord {
    magic: [u8; 8],
    physical_version: U16<LittleEndian>,
    header_bytes: U16<LittleEndian>,
    record_bytes: U16<LittleEndian>,
    reserved: U16<LittleEndian>,
    checksum: [u8; CHECKSUM_BYTES],
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout)]
pub(crate) struct FrameRecord {
    sequence: U64<LittleEndian>,
    pub(crate) record: WorkflowRecord,
    checksum: [u8; CHECKSUM_BYTES],
}

pub const JOURNAL_HEADER_BYTES: usize = size_of::<HeaderRecord>();
pub const JOURNAL_FRAME_BYTES: usize = size_of::<FrameRecord>();
const HEADER_PAYLOAD_BYTES: usize = JOURNAL_HEADER_BYTES - CHECKSUM_BYTES;
const FRAME_PAYLOAD_BYTES: usize = JOURNAL_FRAME_BYTES - CHECKSUM_BYTES;

impl HeaderRecord {
    pub(crate) fn canonical() -> Self {
        let mut header = Self {
            magic: MAGIC,
            physical_version: U16::new(PHYSICAL_VERSION),
            header_bytes: U16::new(JOURNAL_HEADER_BYTES as u16),
            record_bytes: U16::new(WORKFLOW_RECORD_BYTES as u16),
            reserved: U16::new(0),
            checksum: [0; CHECKSUM_BYTES],
        };
        header.checksum = checksum(HEADER_DOMAIN, &header.as_bytes()[..HEADER_PAYLOAD_BYTES]);
        header
    }

    pub(crate) fn decode(bytes: [u8; JOURNAL_HEADER_BYTES]) -> Result<Self, HeaderError> {
        let header: Self = zerocopy::transmute!(bytes);
        header.validate()?;
        Ok(header)
    }

    fn validate(&self) -> Result<(), HeaderError> {
        if self.magic != MAGIC {
            return Err(HeaderError::Magic {
                observed: self.magic,
            });
        }
        let physical_version = self.physical_version.get();
        if physical_version != PHYSICAL_VERSION {
            return Err(HeaderError::PhysicalVersion {
                observed: physical_version,
            });
        }
        if self.header_bytes.get() != JOURNAL_HEADER_BYTES as u16 {
            return Err(HeaderError::HeaderWidth {
                expected: JOURNAL_HEADER_BYTES as u16,
                observed: self.header_bytes.get(),
            });
        }
        if self.record_bytes.get() != WORKFLOW_RECORD_BYTES as u16 {
            return Err(HeaderError::RecordWidth {
                expected: WORKFLOW_RECORD_BYTES as u16,
                observed: self.record_bytes.get(),
            });
        }
        let reserved = self.reserved.get();
        if reserved != 0 {
            return Err(HeaderError::Reserved { observed: reserved });
        }
        if self.checksum != checksum(HEADER_DOMAIN, &self.as_bytes()[..HEADER_PAYLOAD_BYTES]) {
            return Err(HeaderError::Checksum);
        }
        Ok(())
    }
}

impl FrameRecord {
    pub(crate) fn encode(sequence: FrameSequence, record: WorkflowRecord) -> Self {
        let mut frame = Self {
            sequence: U64::new(*sequence),
            record,
            checksum: [0; CHECKSUM_BYTES],
        };
        frame.checksum = frame_checksum(&frame.as_bytes()[..FRAME_PAYLOAD_BYTES]);
        frame
    }

    pub(crate) fn decode(bytes: [u8; JOURNAL_FRAME_BYTES]) -> Self {
        zerocopy::transmute!(bytes)
    }

    pub(crate) fn sequence(&self) -> FrameSequence {
        FrameSequence::from(self.sequence.get())
    }

    pub(crate) fn checksum_is_valid(&self) -> bool {
        self.checksum == frame_checksum(&self.as_bytes()[..FRAME_PAYLOAD_BYTES])
    }
}

pub(crate) fn frame_offset(sequence: FrameSequence) -> Option<JournalOffset> {
    sequence
        .checked_mul(JOURNAL_FRAME_BYTES as u64)
        .and_then(|frames| frames.checked_add(JOURNAL_HEADER_BYTES as u64))
        .map(JournalOffset::from)
}

fn frame_checksum(payload: &[u8]) -> [u8; CHECKSUM_BYTES] {
    let mut hasher = Hasher::new();
    hasher.update(FRAME_DOMAIN);
    hasher.update(&PHYSICAL_VERSION.to_le_bytes());
    hasher.update(payload);
    truncate(hasher.finalize())
}

fn checksum(domain: &[u8], bytes: &[u8]) -> [u8; CHECKSUM_BYTES] {
    let mut hasher = Hasher::new();
    hasher.update(domain);
    hasher.update(bytes);
    truncate(hasher.finalize())
}

fn truncate(hash: blake3::Hash) -> [u8; CHECKSUM_BYTES] {
    let mut checksum = [0; CHECKSUM_BYTES];
    checksum.copy_from_slice(&hash.as_bytes()[..CHECKSUM_BYTES]);
    checksum
}

const _: () = {
    assert!(JOURNAL_HEADER_BYTES == 32);
    assert!(JOURNAL_FRAME_BYTES == 92);
    assert!(WORKFLOW_RECORD_BYTES == 68);
};

#[cfg(test)]
mod tests {
    use nudox_workflow::{EventKind, StageKey, WorkflowEvent, WorkflowRecord, WorkflowVersion};
    use zerocopy::IntoBytes;

    use super::{FRAME_PAYLOAD_BYTES, FrameRecord};
    use crate::FrameSequence;

    #[test]
    fn every_payload_byte_is_covered_by_the_frame_checksum() {
        let record = WorkflowRecord::from(WorkflowEvent {
            version: WorkflowVersion::WAVE1,
            key: StageKey::from([19; 32]),
            kind: EventKind::Requested,
        });
        let canonical = FrameRecord::encode(FrameSequence::FIRST, record);
        for offset in 0..FRAME_PAYLOAD_BYTES {
            let mut bytes = [0; super::JOURNAL_FRAME_BYTES];
            bytes.copy_from_slice(canonical.as_bytes());
            bytes[offset] ^= 1;
            assert!(!FrameRecord::decode(bytes).checksum_is_valid());
        }
    }
}
