//! Typed encoding for the atomically published latest-generation facts.

use core::mem::size_of;

use super::{
    errors::{PublicationGenerationError, PublicationSnapshotError},
    facts::{ImmutablePublicationIdentity, PublicationFacts, PublicationHeadIdentity},
    format::PublicationInput,
    snapshot::{AtomicSnapshot, SnapshotReadError, SnapshotWriteError},
};
use crate::{FrameSequence, JournalOffset, ReceiptFacts};

const PUBLICATION_BYTES: usize = 112;
const PUBLICATION_WORDS: usize = PUBLICATION_BYTES / size_of::<u64>();

pub(super) struct LatestPublication {
    snapshot: AtomicSnapshot<PUBLICATION_WORDS>,
}

impl core::fmt::Debug for LatestPublication {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("LatestPublication")
            .finish_non_exhaustive()
    }
}

impl LatestPublication {
    pub(super) fn empty() -> Self {
        Self {
            snapshot: AtomicSnapshot::empty(),
        }
    }

    pub(super) fn replace(
        &self,
        attempted: PublicationFacts,
    ) -> Result<(), PublicationSnapshotError> {
        self.snapshot
            .replace(encode(attempted))
            .map_err(|source| match source {
                SnapshotWriteError::ConcurrentWriter { observed_version } => {
                    PublicationSnapshotError::ConcurrentWriter {
                        observed_version,
                        attempted,
                    }
                }
                SnapshotWriteError::VersionExhausted { observed_version } => {
                    PublicationSnapshotError::VersionExhausted {
                        observed_version,
                        attempted,
                    }
                }
            })
    }

    pub(super) fn read(&self) -> Result<Option<PublicationFacts>, LatestPublicationReadError> {
        self.snapshot
            .read()
            .map_err(
                |SnapshotReadError {
                     attempts,
                     observed_version,
                 }| {
                    LatestPublicationReadError::Snapshot(PublicationSnapshotError::ReadContended {
                        attempts,
                        observed_version,
                    })
                },
            )?
            .map(decode)
            .transpose()
    }
}

pub(super) enum LatestPublicationReadError {
    Snapshot(PublicationSnapshotError),
    Generation(PublicationGenerationError),
}

fn encode(facts: PublicationFacts) -> [u64; PUBLICATION_WORDS] {
    let mut bytes = [0_u8; PUBLICATION_BYTES];
    bytes[0..32].copy_from_slice(facts.generation.pinned_root.as_ref());
    bytes[32..64].copy_from_slice(facts.generation.dep_set.as_ref());
    bytes[64..72].copy_from_slice(&u64::to_le_bytes(*facts.stable.sequence));
    bytes[72..80].copy_from_slice(&u64::to_le_bytes(*facts.stable.durable_end));
    bytes[80..96].copy_from_slice(&facts.immutable.checksum);
    bytes[96..112].copy_from_slice(&facts.head.checksum);
    core::array::from_fn(|index| {
        let start = index * size_of::<u64>();
        u64::from_le_bytes([
            bytes[start],
            bytes[start + 1],
            bytes[start + 2],
            bytes[start + 3],
            bytes[start + 4],
            bytes[start + 5],
            bytes[start + 6],
            bytes[start + 7],
        ])
    })
}

fn decode(words: [u64; PUBLICATION_WORDS]) -> Result<PublicationFacts, LatestPublicationReadError> {
    let mut bytes = [0_u8; PUBLICATION_BYTES];
    for (index, word) in words.into_iter().enumerate() {
        let start = index * size_of::<u64>();
        bytes[start..start + size_of::<u64>()].copy_from_slice(&word.to_le_bytes());
    }
    let root = copy_array::<32>(&bytes, 0);
    let dep_set = copy_array::<32>(&bytes, 32);
    let generation = PublicationInput::from_parts(root, dep_set)
        .generation_facts()
        .map_err(LatestPublicationReadError::Generation)?;
    Ok(PublicationFacts {
        generation,
        stable: ReceiptFacts {
            sequence: FrameSequence::from(u64::from_le_bytes(copy_array::<8>(&bytes, 64))),
            durable_end: JournalOffset::from(u64::from_le_bytes(copy_array::<8>(&bytes, 72))),
        },
        immutable: ImmutablePublicationIdentity {
            checksum: copy_array::<16>(&bytes, 80),
        },
        head: PublicationHeadIdentity {
            checksum: copy_array::<16>(&bytes, 96),
        },
    })
}

fn copy_array<const BYTES: usize>(source: &[u8; PUBLICATION_BYTES], start: usize) -> [u8; BYTES] {
    let mut output = [0_u8; BYTES];
    output.copy_from_slice(&source[start..start + BYTES]);
    output
}
