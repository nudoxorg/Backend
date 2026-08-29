use core::{mem::size_of, ops::Deref};

use blake3::Hasher;
use nudox_workflow::{WORKFLOW_RECORD_BYTES, WorkflowRecord};
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout,
    byteorder::{LittleEndian, U16, U64},
};

use crate::{FrameSequence, HeaderError, JournalOffset};

const MAGIC: [u8; 8] = *b"NUDXJNL\0";
const PHYSICAL_VERSION: u16 = 1;
const HEADER_WIDTH: u16 = 32;
const WORKFLOW_RECORD_WIDTH: u16 = 68;
const CHECKSUM_BYTES: usize = 16;

#[repr(transparent)]
struct ChecksumDomain<const BYTES: usize>([u8; BYTES]);

impl<const BYTES: usize> Deref for ChecksumDomain<BYTES> {
    type Target = [u8; BYTES];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<const BYTES: usize> AsRef<[u8]> for ChecksumDomain<BYTES> {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

const HEADER_DOMAIN: ChecksumDomain<24> = ChecksumDomain(*b"nudox.journal.header.v1\0");
const FRAME_DOMAIN: ChecksumDomain<23> = ChecksumDomain(*b"nudox.journal.frame.v1\0");

#[repr(C)]
#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout)]
struct HeaderFields {
    magic: [u8; 8],
    physical_version: U16<LittleEndian>,
    header_bytes: U16<LittleEndian>,
    record_bytes: U16<LittleEndian>,
    reserved: U16<LittleEndian>,
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout)]
pub(crate) struct HeaderRecord {
    fields: HeaderFields,
    checksum: [u8; CHECKSUM_BYTES],
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout)]
struct FramePayload {
    sequence: U64<LittleEndian>,
    record: WorkflowRecord,
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout)]
pub(crate) struct FrameRecord {
    payload: FramePayload,
    checksum: [u8; CHECKSUM_BYTES],
}

pub const JOURNAL_HEADER_BYTES: usize = size_of::<HeaderRecord>();
pub const JOURNAL_FRAME_BYTES: usize = size_of::<FrameRecord>();

impl HeaderRecord {
    pub(crate) fn canonical() -> Self {
        let fields = HeaderFields {
            magic: MAGIC,
            physical_version: U16::new(PHYSICAL_VERSION),
            header_bytes: U16::new(HEADER_WIDTH),
            record_bytes: U16::new(WORKFLOW_RECORD_WIDTH),
            reserved: U16::new(0),
        };
        Self {
            checksum: checksum(&HEADER_DOMAIN, fields.as_bytes()),
            fields,
        }
    }

    pub(crate) fn decode(bytes: [u8; JOURNAL_HEADER_BYTES]) -> Result<Self, HeaderError> {
        let header: Self = zerocopy::transmute!(bytes);
        header.validate()?;
        Ok(header)
    }

    fn validate(&self) -> Result<(), HeaderError> {
        let fields = &self.fields;
        if fields.magic != MAGIC {
            return Err(HeaderError::Magic {
                observed: fields.magic,
            });
        }
        let physical_version = fields.physical_version.get();
        if physical_version != PHYSICAL_VERSION {
            return Err(HeaderError::PhysicalVersion {
                observed: physical_version,
            });
        }
        require_width(
            fields.header_bytes.get(),
            HEADER_WIDTH,
            |expected, observed| HeaderError::HeaderWidth { expected, observed },
        )?;
        require_width(
            fields.record_bytes.get(),
            WORKFLOW_RECORD_WIDTH,
            |expected, observed| HeaderError::RecordWidth { expected, observed },
        )?;
        let reserved = fields.reserved.get();
        if reserved != 0 {
            return Err(HeaderError::Reserved { observed: reserved });
        }
        if self.checksum != checksum(&HEADER_DOMAIN, fields.as_bytes()) {
            return Err(HeaderError::Checksum);
        }
        Ok(())
    }
}

impl FrameRecord {
    pub(crate) fn encode(sequence: FrameSequence, record: WorkflowRecord) -> Self {
        let payload = FramePayload {
            sequence: U64::new(sequence.0),
            record,
        };
        Self {
            checksum: frame_checksum(&payload),
            payload,
        }
    }

    pub(crate) fn decode(bytes: [u8; JOURNAL_FRAME_BYTES]) -> Self {
        zerocopy::transmute!(bytes)
    }

    pub(crate) fn sequence(&self) -> FrameSequence {
        FrameSequence(self.payload.sequence.get())
    }

    pub(crate) fn record(&self) -> WorkflowRecord {
        self.payload.record
    }

    pub(crate) fn checksum_is_valid(&self) -> bool {
        self.checksum == frame_checksum(&self.payload)
    }
}

pub(crate) fn frame_offset(sequence: FrameSequence) -> Option<JournalOffset> {
    sequence
        .0
        .checked_mul(JOURNAL_FRAME_BYTES as u64)
        .and_then(|frames| frames.checked_add(JOURNAL_HEADER_BYTES as u64))
        .map(JournalOffset)
}

pub(crate) fn receipt_end(sequence: FrameSequence) -> Option<JournalOffset> {
    sequence
        .0
        .checked_add(1)
        .and_then(|count| count.checked_mul(JOURNAL_FRAME_BYTES as u64))
        .and_then(|frames| frames.checked_add(JOURNAL_HEADER_BYTES as u64))
        .map(JournalOffset)
}

fn frame_checksum(payload: &FramePayload) -> [u8; CHECKSUM_BYTES] {
    let mut hasher = Hasher::new();
    hasher.update(FRAME_DOMAIN.as_ref());
    hasher.update(&PHYSICAL_VERSION.to_le_bytes());
    hasher.update(payload.as_bytes());
    truncate(hasher.finalize())
}

fn checksum<const DOMAIN_BYTES: usize>(
    domain: &ChecksumDomain<DOMAIN_BYTES>,
    bytes: &[u8],
) -> [u8; CHECKSUM_BYTES] {
    let mut hasher = Hasher::new();
    hasher.update(domain.as_ref());
    hasher.update(bytes);
    truncate(hasher.finalize())
}

fn truncate(hash: blake3::Hash) -> [u8; CHECKSUM_BYTES] {
    let mut checksum = [0; CHECKSUM_BYTES];
    checksum.copy_from_slice(&hash.as_bytes()[..CHECKSUM_BYTES]);
    checksum
}

fn require_width(
    observed: u16,
    expected: u16,
    error: impl FnOnce(u16, u16) -> HeaderError,
) -> Result<(), HeaderError> {
    if observed == expected {
        Ok(())
    } else {
        Err(error(expected, observed))
    }
}

const _: () = {
    assert!(JOURNAL_HEADER_BYTES == HEADER_WIDTH as usize);
    assert!(JOURNAL_FRAME_BYTES == 92);
    assert!(WORKFLOW_RECORD_BYTES == WORKFLOW_RECORD_WIDTH as usize);
};
